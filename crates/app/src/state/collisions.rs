//! Collision view of the EQ editor (T32): where the EQ's track collides with other tracks
//! (same rule as the Mix assistant's masking: loud in the same octave band at the same
//! time, at similar levels), drawn on the EQ display. The data is the Mix assistant's
//! report (`cadenza_mcp::tools::mix::run_analysis`, offline, on its worker): a fresh one
//! is reused, else "Show collisions" starts one; a report of an older document is shown
//! with a stale hint. A click on a collision proposes a bell cut (dashed, like the match
//! EQ proposal) that Apply commits as ONE undo step.

use crate::i18n::trf;
use std::sync::Arc;

use cadenza_analysis::issues::{TrackInput, track_collisions};
use cadenza_analysis::masking::{CUT_Q, Collision};
use cadenza_core::TrackId;
use cadenza_mcp::tools::mix::{ChannelAnalysis, MixAnalysis};
use cadenza_model::TrackKind;

use super::AppState;
use super::eq_editor::{BANDS, BandKind, Change, DORMANT_DB, EqValues, clamp};
use super::match_eq::analyzer_bins;
use super::mix::octave_range;
use super::selection::DeviceRef;

/// Tracks drawn as thin spectra next to this one.
pub const MAX_SPECTRA: usize = 3;
/// Where the loudest drawn spectrum sits on the analyzer scale (dB).
const SPECTRA_TOP_DB: f32 = -12.0;

/// What the collision view of one EQ can show.
#[derive(Debug, Clone)]
pub enum CollisionStatus {
	/// No track to compare (master or bus EQ): the reason.
	Unavailable(String),
	/// The analysis is rendering (fraction done).
	Running(f32),
	/// A report (`stale`: the document changed since).
	Ready {
		report: Arc<MixAnalysis>,
		stale: bool,
	},
	Failed(String),
	/// Not analyzed yet (nothing running).
	Idle,
}

/// One octave band where others collide with this track: the strongest collision there.
#[derive(Debug, Clone, PartialEq)]
pub struct CollisionBand {
	pub collision: Collision,
	pub low_hz: f32,
	pub high_hz: f32,
	/// 0..1 (the masking score).
	pub heat: f32,
	/// Every other track colliding in this band (indices into the report's channels).
	pub others: Vec<usize>,
}

/// A proposed cut on the EQ (dashed until Apply).
#[derive(Debug, Clone, PartialEq)]
pub struct CutSuggestion {
	pub device: DeviceRef,
	pub collision: Collision,
	pub changes: Vec<Change>,
	pub label: String,
}

/// The collision view's flow (one EQ at a time).
#[derive(Default)]
pub struct CollisionState {
	/// The EQ showing its collisions.
	pub shown: Option<DeviceRef>,
	pub suggestion: Option<CutSuggestion>,
	/// The running mix analysis was started by the view (hiding it cancels it).
	started: bool,
}

/// Collisions of `track` in an analysis (`Collision::other` indexes `channels`).
pub fn collisions_of(channels: &[ChannelAnalysis], track: TrackId) -> Vec<Collision> {
	let inputs: Vec<TrackInput<'_>> = channels
		.iter()
		.map(|c| TrackInput {
			id: c.track_id,
			name: &c.name,
			role: c.role,
			is_bus: c.kind == TrackKind::Bus,
			detail: &c.detail,
			pan: c.pan,
			high_pass_hz: c.high_pass_hz,
		})
		.collect();
	track_collisions(&inputs, track)
}

/// One entry per octave band with collisions, low to high.
pub fn bands_of(collisions: &[Collision]) -> Vec<CollisionBand> {
	let mut out: Vec<CollisionBand> = Vec::new();
	// Strongest first, so the first of each band is its top collision.
	for c in collisions {
		match out.iter_mut().find(|b| b.collision.band == c.band) {
			Some(b) => {
				if !b.others.contains(&c.other) {
					b.others.push(c.other);
				}
			}
			None => out.push(CollisionBand {
				collision: c.clone(),
				low_hz: c.band_hz / std::f32::consts::SQRT_2,
				high_hz: c.band_hz * std::f32::consts::SQRT_2,
				heat: c.score.clamp(0.0, 1.0),
				others: vec![c.other],
			}),
		}
	}
	out.sort_by_key(|b| b.collision.band);
	out
}

/// The band under `freq`.
pub fn band_at(bands: &[CollisionBand], freq: f32) -> Option<&CollisionBand> {
	bands.iter().find(|b| (b.low_hz..b.high_hz).contains(&freq))
}

/// The other tracks colliding most (best score each), at most `n`.
pub fn top_others(collisions: &[Collision], n: usize) -> Vec<usize> {
	let mut out = Vec::new();
	for c in collisions {
		if !out.contains(&c.other) {
			out.push(c.other);
		}
		if out.len() == n {
			break;
		}
	}
	out
}

/// Mean third-octave spectra at the analyzer points: this track's, then each of
/// `others`', all moved by the same amount so the loudest sits at -12 dB (relative
/// levels kept).
pub fn spectra(
	channels: &[ChannelAnalysis],
	track: TrackId,
	others: &[usize],
) -> (Vec<f32>, Vec<Vec<f32>>) {
	let own = channels.iter().find(|c| c.track_id == track);
	let thirds = |c: &ChannelAnalysis| c.detail.spectrum.third_octave_db.clone();
	let all: Vec<Vec<f32>> = own
		.map(thirds)
		.into_iter()
		.chain(others.iter().filter_map(|&i| channels.get(i)).map(thirds))
		.collect();
	let top = all.iter().flatten().copied().fold(f32::MIN, f32::max);
	let lift = if top > -144.0 {
		SPECTRA_TOP_DB - top
	} else {
		0.0
	};
	let own_bins = own.map_or_else(Vec::new, |c| {
		analyzer_bins(&c.detail.spectrum.third_octave_db, lift)
	});
	let other_bins = others
		.iter()
		.filter_map(|&i| channels.get(i))
		.map(|c| analyzer_bins(&c.detail.spectrum.third_octave_db, lift))
		.collect();
	(own_bins, other_bins)
}

/// "Bass × Kick 44–89 Hz".
pub fn pair_label(own: &str, other: &str, band_hz: f32) -> String {
	format!("{own} × {other} {}", octave_range(band_hz))
}

/// The bell cut at a collision: the peak band already near it (within a third of an
/// octave), else a free one, else the least used; gain from the masking rule (half of it
/// when the rule says the other track should make room), never shallower than a cut the
/// band already makes there.
pub fn cut_changes(values: &EqValues, c: &Collision) -> Vec<Change> {
	let peaks: Vec<usize> = (0..BANDS.len())
		.filter(|&b| BANDS[b].kind == BandKind::Peak)
		.collect();
	let gain_of = |b: usize| BANDS[b].gain.map_or(0.0, |g| values.get(g));
	let near = peaks.iter().copied().find(|&b| {
		gain_of(b).abs() > DORMANT_DB
			&& (values.get(BANDS[b].freq) / c.center_hz).log2().abs() < 1.0 / 3.0
	});
	let free = peaks
		.iter()
		.copied()
		.find(|&b| gain_of(b).abs() <= DORMANT_DB);
	let Some(band) = near.or(free).or_else(|| {
		peaks
			.iter()
			.copied()
			.min_by(|&a, &b| gain_of(a).abs().total_cmp(&gain_of(b).abs()))
	}) else {
		return Vec::new();
	};
	let b = &BANDS[band];
	let mut gain = if c.cut_here { c.cut_db } else { c.cut_db * 0.5 };
	if near == Some(band) {
		gain = gain.min(gain_of(band));
	}
	let mut out = vec![(b.freq, clamp(b.freq, c.center_hz))];
	if let Some(g) = b.gain {
		out.push((g, clamp(g, gain)));
	}
	if let Some(q) = b.q {
		out.push((q, clamp(q, CUT_Q)));
	}
	out
}

impl AppState {
	/// Whether `r`'s EQ display shows collisions.
	pub fn collisions_shown(&self, r: DeviceRef) -> bool {
		self.collisions.shown == Some(r)
	}

	/// Why `r` has no collision view (its track is the master, a bus or gone).
	fn collision_unavailable(&self, r: DeviceRef) -> Option<String> {
		let Some(id) = r.track else {
			return Some(crate::i18n::tr("The master has no other tracks to collide with").into());
		};
		match self.project().track(id) {
			None => Some(crate::i18n::tr("The track is gone").into()),
			Some(t) if matches!(t.kind, TrackKind::Bus | TrackKind::Folder) => Some(
				crate::i18n::tr("Collisions are measured between source tracks, not buses").into(),
			),
			Some(_) => None,
		}
	}

	/// "Show collisions": reuses a fresh Mix assistant report, else starts one (unless
	/// the Mix assistant is already measuring: its report is used when it lands).
	pub fn show_collisions(&mut self, r: DeviceRef) {
		self.collisions.shown = Some(r);
		self.collisions.suggestion = None;
		if self.collision_unavailable(r).is_some() {
			return;
		}
		let fresh = self.mix.report.is_some() && !self.mix_report_stale();
		if !fresh && !self.mix.busy() {
			self.mix_analyze();
			self.collisions.started = self.mix.job.is_some();
		}
	}

	/// Measures again (the stale hint's button).
	pub fn refresh_collisions(&mut self) {
		if let Some(r) = self.collisions.shown
			&& !self.mix.busy()
		{
			self.collisions.suggestion = None;
			self.mix_analyze();
			self.collisions.started = self.mix.job.is_some();
			self.collisions.shown = Some(r);
		}
	}

	/// Hides the view (and cancels the analysis it started).
	pub fn hide_collisions(&mut self) {
		self.collisions.shown = None;
		self.collisions.suggestion = None;
		if std::mem::take(&mut self.collisions.started) && self.mix.job.is_some() {
			self.mix_cancel();
		}
	}

	pub fn toggle_collisions(&mut self, r: DeviceRef) {
		if self.collisions_shown(r) {
			self.hide_collisions();
		} else {
			self.show_collisions(r);
		}
	}

	/// What the view of `r` can draw.
	pub fn collision_status(&self, r: DeviceRef) -> CollisionStatus {
		if let Some(reason) = self.collision_unavailable(r) {
			return CollisionStatus::Unavailable(reason);
		}
		if let Some((_, f)) = self.mix.analyzing() {
			return CollisionStatus::Running(f);
		}
		if let Some(report) = &self.mix.report {
			return CollisionStatus::Ready {
				report: report.clone(),
				stale: self.mix_report_stale(),
			};
		}
		match &self.mix.error {
			Some(e) => CollisionStatus::Failed(e.clone()),
			None => CollisionStatus::Idle,
		}
	}

	/// Proposes the cut of `collision` on `r` (shown dashed until Apply / Cancel).
	pub fn suggest_collision_cut(&mut self, r: DeviceRef, collision: &Collision) {
		let Some(track) = r.track else {
			return;
		};
		let values = EqValues::from_fn(|p| {
			self.param_value(r.track, r.device, p.id)
				.unwrap_or(p.default)
		});
		let changes = cut_changes(&values, collision);
		if changes.is_empty() {
			return;
		}
		let own = self
			.project()
			.track(track)
			.map_or_else(String::new, |t| t.name.clone());
		let other = self
			.mix
			.report
			.as_ref()
			.and_then(|rep| rep.channels.get(collision.other))
			.map_or_else(String::new, |c| c.name.clone());
		let label = trf(
			"Cut '{own}' under '{other}' at {hz}",
			&[
				("own", &own),
				("other", &other),
				("hz", &hz_text(collision.center_hz)),
			],
		);
		self.collisions.suggestion = Some(CutSuggestion {
			device: r,
			collision: collision.clone(),
			changes,
			label,
		});
	}

	pub fn collision_suggestion(&self, r: DeviceRef) -> Option<&CutSuggestion> {
		self.collisions
			.suggestion
			.as_ref()
			.filter(|s| s.device == r)
	}

	/// Commits the proposed cut as ONE undo step.
	pub fn apply_collision_cut(&mut self) -> bool {
		let Some(s) = self.collisions.suggestion.take() else {
			return false;
		};
		self.commit_params(s.device.track, s.device.device, &s.changes, &s.label)
	}

	pub fn cancel_collision_cut(&mut self) {
		self.collisions.suggestion = None;
	}

	/// The mix report landed: the view no longer owns a running job.
	pub(super) fn poll_collisions(&mut self) {
		if self.collisions.started && self.mix.job.is_none() {
			self.collisions.started = false;
		}
	}
}

/// "63 Hz", "2.5 kHz".
pub fn hz_text(hz: f32) -> String {
	if hz >= 1_000.0 {
		format!(
			"{} kHz",
			crate::i18n::decimal(format!("{:.1}", hz / 1_000.0))
		)
	} else {
		format!("{hz:.0} Hz")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_analysis::detail::Detail;
	use cadenza_analysis::roles::Role;
	use cadenza_analysis::spectrum::{Spectrum, Stereo, Timeline};
	use cadenza_analysis::{BandEnergy, LevelStats};
	use cadenza_model::{Command, Device, Track};

	fn detail(octaves: [f32; 10]) -> Detail {
		let thirds: Vec<f32> = (0..31)
			.map(|i| octaves[cadenza_analysis::spectrum::octave_of_third(i).min(9)])
			.collect();
		Detail {
			stats: LevelStats {
				peak_db: -3.0,
				true_peak_db: -3.0,
				rms_db: -15.0,
				lufs_integrated: -14.0,
				lufs_short_term_max: -12.0,
				crest_factor_db: 12.0,
				clipped_samples: 0,
				band_energy_db: BandEnergy {
					low: -10.0,
					low_mid: -10.0,
					mid: -10.0,
					high_mid: -10.0,
					high: -10.0,
				},
				stereo_correlation: 0.9,
			},
			spectrum: Spectrum {
				third_octave_db: thirds.clone(),
				side_third_octave_db: thirds,
				octave_db: octaves.to_vec(),
			},
			stereo: Stereo {
				width: 0.1,
				low_correlation: 1.0,
				low_side_db: -40.0,
			},
			timeline: Timeline {
				hop_seconds: 0.085,
				frames: vec![octaves; 40],
			},
		}
	}

	fn channel(id: TrackId, name: &str, role: Role, octaves: [f32; 10]) -> ChannelAnalysis {
		ChannelAnalysis {
			track_id: id,
			name: name.into(),
			kind: TrackKind::Instrument,
			role,
			volume_db: 0.0,
			pan: 0.0,
			high_pass_hz: None,
			detail: detail(octaves),
		}
	}

	const S: f32 = -144.0;

	fn channels(bass: TrackId) -> Vec<ChannelAnalysis> {
		vec![
			channel(
				TrackId::new(),
				"Kick",
				Role::Kick,
				[
					S, -12.0, -20.0, -40.0, -50.0, -60.0, -40.0, -45.0, -60.0, -80.0,
				],
			),
			channel(
				bass,
				"Bass",
				Role::Bass,
				[
					S, -14.0, -12.0, -25.0, -40.0, -60.0, -70.0, -80.0, -90.0, -100.0,
				],
			),
			channel(
				TrackId::new(),
				"Keys",
				Role::Keys,
				[S, S, -30.0, -14.0, -16.0, -20.0, -30.0, -50.0, -70.0, S],
			),
		]
	}

	#[test]
	fn bands_name_the_strongest_collision_and_every_other_track() {
		let bass = TrackId::new();
		let ch = channels(bass);
		let c = collisions_of(&ch, bass);
		assert!(!c.is_empty());
		let bands = bands_of(&c);
		assert!(
			bands
				.windows(2)
				.all(|w| w[0].collision.band < w[1].collision.band)
		);
		let low = bands
			.iter()
			.find(|b| b.collision.band_hz == 63.0)
			.expect("63 Hz");
		assert_eq!(low.others, vec![0], "the kick");
		assert!(low.heat > 0.0 && low.heat <= 1.0);
		assert_eq!(band_at(&bands, 60.0).map(|b| b.collision.band), Some(1));
		assert!(band_at(&bands, 15_000.0).is_none());
		let top = top_others(&c, MAX_SPECTRA);
		assert_eq!(top[0], c[0].other);
		let (own, others) = spectra(&ch, bass, &top);
		assert_eq!(own.len(), super::super::device_display::ANALYZER_BINS);
		assert_eq!(others.len(), top.len());
		let loudest = own
			.iter()
			.chain(others.iter().flatten())
			.copied()
			.fold(f32::MIN, f32::max);
		assert!((loudest - SPECTRA_TOP_DB).abs() < 0.5, "{loudest}");
		assert_eq!(pair_label("Bass", "Kick", 63.0), "Bass × Kick 45–89 Hz");
	}

	#[test]
	fn the_cut_takes_a_free_peak_or_deepens_a_near_one() {
		let c = Collision {
			other: 0,
			band: 1,
			band_hz: 63.0,
			center_hz: 80.0,
			overlap: 1.0,
			score: 0.8,
			level_db: -14.0,
			other_level_db: -12.0,
			cut_here: true,
			cut_db: -4.0,
		};
		let flat = EqValues::default();
		assert_eq!(
			cut_changes(&flat, &c),
			vec![
				("peak1_freq", 80.0),
				("peak1_gain", -4.0),
				("peak1_q", CUT_Q)
			]
		);
		// Peak 1 busy elsewhere: Peak 2; the other track should make room: half the cut.
		let mut v = EqValues::default();
		v.apply(&[("peak1_freq", 2_000.0), ("peak1_gain", 3.0)]);
		let other = Collision {
			cut_here: false,
			..c.clone()
		};
		assert_eq!(cut_changes(&v, &other)[1], ("peak2_gain", -2.0));
		// A deeper cut already near: kept.
		v.apply(&[("peak2_freq", 85.0), ("peak2_gain", -6.0)]);
		assert_eq!(cut_changes(&v, &c)[1], ("peak2_gain", -6.0));
	}

	#[test]
	fn a_suggested_cut_applies_as_one_undo_step() {
		let mut s = AppState::headless();
		let mut track = Track::new("Bass", TrackKind::Instrument);
		let eq = Device::builtin("eq", "EQ");
		let r = DeviceRef {
			track: Some(track.id),
			device: eq.id,
		};
		track.channel.inserts.push(eq);
		let bass = track.id;
		assert!(s.apply(Command::AddTrack { track, index: None }));
		assert!(matches!(
			s.collision_status(DeviceRef {
				track: None,
				device: r.device
			}),
			CollisionStatus::Unavailable(_)
		));
		assert!(matches!(s.collision_status(r), CollisionStatus::Idle));
		let c = collisions_of(&channels(bass), bass)[0].clone();
		s.collisions.shown = Some(r);
		s.suggest_collision_cut(r, &c);
		let sug = s.collision_suggestion(r).expect("a suggestion").clone();
		assert!(
			sug.changes
				.iter()
				.any(|(id, g)| *id == "peak1_gain" && *g < 0.0)
		);
		let steps = s.document().history_labels().len();
		assert!(s.apply_collision_cut());
		assert!(s.collision_suggestion(r).is_none());
		assert_eq!(s.document().history_labels().len(), steps + 1);
		let gain = s.project().device(r.track, r.device).unwrap().params["peak1_gain"];
		assert_eq!(gain, sug.changes[1].1);
		s.undo();
		assert_eq!(s.document().history_labels().len(), steps);
		s.suggest_collision_cut(r, &c);
		s.cancel_collision_cut();
		assert!(!s.apply_collision_cut());
	}
}
