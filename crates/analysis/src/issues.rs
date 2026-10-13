//! Mix issues: what the measurements mean against the targets, each with a ready-made fix
//! (a list of [`MixMove`]s) the AI or the app can apply as is.

use cadenza_core::TrackId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::detail::Detail;
use crate::masking::{MaskInput, MaskingPair, masking};
use crate::moves::{EqKind, MixMove, hz};
use crate::roles::Role;
use crate::targets::{LOUDNESS_TOLERANCE_LU, MixTargets};

#[derive(
	Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
	High,
	Medium,
	Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
	Clipping,
	TruePeak,
	Loudness,
	Phase,
	NarrowImage,
	WideLowEnd,
	ToneBalance,
	OverCompressed,
	TrackHot,
	Balance,
	Rumble,
	Masking,
	EngineWarning,
}

/// One problem and its fix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Issue {
	/// "i1", "i2"... in report order (most severe first).
	pub id: String,
	pub severity: Severity,
	pub kind: IssueKind,
	/// The channel concerned (`None` = master / whole mix).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub track_id: Option<TrackId>,
	pub message: String,
	/// The fix: pass these to apply_mix_moves (empty = advice only).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub moves: Vec<MixMove>,
}

/// One channel of the analyzed mix.
#[derive(Debug, Clone, Copy)]
pub struct TrackInput<'a> {
	pub id: TrackId,
	pub name: &'a str,
	pub role: Role,
	/// Bus returns are measured but kept out of balance and masking comparisons.
	pub is_bus: bool,
	pub detail: &'a Detail,
	pub pan: f32,
	/// Corner of an active high-pass on the channel's built-in EQ.
	pub high_pass_hz: Option<f32>,
}

/// The master's built-in limiter, when it has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimiterState {
	pub input_gain_db: f32,
	pub ceiling_db: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct MasterInput<'a> {
	pub detail: &'a Detail,
	pub limiter: Option<LimiterState>,
}

/// Masking among the non-silent source tracks, indices into `tracks`.
pub fn track_masking(tracks: &[TrackInput<'_>]) -> Vec<MaskingPair> {
	let sources: Vec<usize> = (0..tracks.len())
		.filter(|&i| !tracks[i].is_bus && !tracks[i].detail.is_silent())
		.collect();
	let inputs: Vec<MaskInput<'_>> = sources
		.iter()
		.map(|&i| MaskInput {
			role: tracks[i].role,
			frames: &tracks[i].detail.timeline.frames,
			octave_db: &tracks[i].detail.spectrum.octave_db,
		})
		.collect();
	masking(&inputs)
		.into_iter()
		.map(|mut p| {
			p.a = sources[p.a];
			p.b = sources[p.b];
			p.cut = sources[p.cut];
			p
		})
		.collect()
}

/// Peaks louder than this on a channel leave no headroom (dBFS).
const HOT_TRACK_DB: f32 = -0.5;
/// A part this far (dB) from its role's expected balance is flagged.
const BALANCE_TOLERANCE_DB: f32 = 6.0;
/// Tonal balance drift (dB vs the profile curve) before flagging.
const TONE_TOLERANCE_DB: f32 = 4.0;
const OVER_COMPRESSED_CREST_DB: f32 = 8.0;
const NEAR_MONO_CORRELATION: f32 = 0.95;
/// Masking pairs reported as issues (the rest stay in the masking list).
const MASKING_ISSUES: usize = 5;
const MASKING_ISSUE_SCORE: f32 = 0.25;

fn round_half(db: f32) -> f32 {
	(db * 2.0).round() / 2.0
}

/// Finds the mix's issues, most severe first. `masking` comes from [`track_masking`];
/// `warnings` are the render's compile warnings.
pub fn find_issues(
	master: &MasterInput<'_>,
	tracks: &[TrackInput<'_>],
	targets: &MixTargets,
	masking: &[MaskingPair],
	warnings: &[String],
) -> Vec<Issue> {
	let mut out: Vec<Issue> = Vec::new();
	let mut push = |severity, kind, track_id, message: String, moves: Vec<MixMove>| {
		out.push(Issue {
			id: String::new(),
			severity,
			kind,
			track_id,
			message,
			moves,
		})
	};
	let m = &master.detail.stats;
	let silent = master.detail.is_silent();
	// A sample-peak limiter: leave room for inter-sample peaks.
	let limit_ceiling = round_half(targets.true_peak_db - 0.5);

	if !silent && (m.clipped_samples > 0 || m.true_peak_db > targets.true_peak_db + 0.05) {
		let clipping = m.clipped_samples > 0;
		let mut message = format!(
			"master true peak {:+.1} dBTP is above the {:.1} dBTP ceiling",
			m.true_peak_db, targets.true_peak_db
		);
		if clipping {
			message.push_str(&format!(" and {} samples clip", m.clipped_samples));
		}
		message.push_str(&format!(": limit the master at {limit_ceiling:.1} dB"));
		push(
			if clipping || m.true_peak_db > 0.0 {
				Severity::High
			} else {
				Severity::Medium
			},
			if clipping {
				IssueKind::Clipping
			} else {
				IssueKind::TruePeak
			},
			None,
			message,
			vec![MixMove::Limiter {
				track_id: None,
				ceiling_db: Some(limit_ceiling),
				input_gain_db: None,
			}],
		);
	}

	let gap = m.lufs_integrated - targets.lufs;
	if !silent && gap.abs() > LOUDNESS_TOLERANCE_LU {
		let mut moves = Vec::new();
		let drive = master.limiter.map(|l| l.input_gain_db);
		if gap < 0.0 {
			let need = round_half(-gap);
			match drive {
				Some(d) => moves.push(MixMove::Limiter {
					track_id: None,
					ceiling_db: Some(limit_ceiling),
					input_gain_db: Some((d + need).min(12.0)),
				}),
				None if m.true_peak_db + need <= targets.true_peak_db => {
					moves.push(MixMove::Gain {
						track_id: None,
						delta_db: Some(need),
						db: None,
					})
				}
				None => moves.push(MixMove::Limiter {
					track_id: None,
					ceiling_db: Some(limit_ceiling),
					input_gain_db: Some(need.min(12.0)),
				}),
			}
		} else {
			let excess = round_half(gap);
			let from_drive = drive.unwrap_or(0.0).min(excess).max(0.0);
			if from_drive > 0.0 {
				moves.push(MixMove::Limiter {
					track_id: None,
					ceiling_db: None,
					input_gain_db: Some(drive.unwrap_or(0.0) - from_drive),
				});
			}
			if excess - from_drive >= 0.5 {
				moves.push(MixMove::Gain {
					track_id: None,
					delta_db: Some(-(excess - from_drive)),
					db: None,
				});
			}
		}
		push(
			if gap.abs() > 4.0 {
				Severity::High
			} else {
				Severity::Medium
			},
			IssueKind::Loudness,
			None,
			format!(
				"master is {:.1} LUFS, {:.1} LU {} than the {:.1} LUFS target",
				m.lufs_integrated,
				gap.abs(),
				if gap > 0.0 { "louder" } else { "quieter" },
				targets.lufs
			),
			moves,
		);
	}

	if !silent && m.stereo_correlation < 0.0 {
		push(
			Severity::High,
			IssueKind::Phase,
			None,
			format!(
				"master L/R correlation is {:.2}: parts cancel in mono; check polarity and \
				 wide stereo effects",
				m.stereo_correlation
			),
			Vec::new(),
		);
	}
	for t in tracks.iter().filter(|t| !t.detail.is_silent()) {
		if t.detail.stats.stereo_correlation < -0.3 {
			push(
				Severity::High,
				IssueKind::Phase,
				Some(t.id),
				format!(
					"'{}' has L/R correlation {:.2}: out of phase, it vanishes in mono",
					t.name, t.detail.stats.stereo_correlation
				),
				Vec::new(),
			);
		}
	}

	let stereo = master.detail.stereo;
	if !silent && m.stereo_correlation > NEAR_MONO_CORRELATION && stereo.width < 0.03 {
		let mut side = -1.0;
		let spread: Vec<&TrackInput<'_>> = tracks
			.iter()
			.filter(|t| !t.is_bus && t.role.spreads() && t.pan.abs() < 0.05)
			.filter(|t| !t.detail.is_silent())
			.take(4)
			.collect();
		let mut moves: Vec<MixMove> = spread
			.iter()
			.map(|t| {
				side = -side;
				MixMove::Pan {
					track_id: t.id,
					pan: -0.35 * side,
				}
			})
			.collect();
		// A shared stereo reverb widens what panning cannot (created when the song has none).
		moves.extend(spread.iter().take(2).map(|t| MixMove::Send {
			track_id: t.id,
			bus_id: None,
			effect: Some(crate::moves::ReturnEffect::Reverb),
			level_db: -18.0,
		}));
		push(
			Severity::Low,
			IssueKind::NarrowImage,
			None,
			format!(
				"the mix is almost mono (width {:.0}%, correlation {:.2}): spread supporting \
				 parts with pan or stereo effects",
				stereo.width * 100.0,
				m.stereo_correlation
			),
			moves,
		);
	}

	if !silent && (stereo.low_correlation < 0.7 || stereo.low_side_db > -12.0) {
		let culprits: Vec<&TrackInput<'_>> = tracks
			.iter()
			.filter(|t| !t.is_bus && !t.detail.is_silent())
			.filter(|t| {
				let d = t.detail;
				let peak = d
					.spectrum
					.octave_db
					.iter()
					.copied()
					.fold(f32::MIN, f32::max);
				let low = d.spectrum.octave_db[1].max(d.spectrum.octave_db[2]);
				d.stereo.low_side_db > -12.0 && low > peak - 12.0
			})
			.collect();
		let names: Vec<&str> = culprits.iter().map(|t| t.name).collect();
		push(
			Severity::Medium,
			IssueKind::WideLowEnd,
			None,
			format!(
				"the low end (below 120 Hz) is not mono (correlation {:.2}, side {:.0} dB){}: \
				 keep bass and kick centered and mono",
				stereo.low_correlation,
				stereo.low_side_db,
				if names.is_empty() {
					String::new()
				} else {
					format!("; wide lows in {}", names.join(", "))
				}
			),
			culprits
				.iter()
				.filter(|t| t.pan.abs() > 0.05)
				.map(|t| MixMove::Pan {
					track_id: t.id,
					pan: 0.0,
				})
				.collect(),
		);
	}

	if !silent {
		let b = &m.band_energy_db;
		let curve = targets.profile.curve();
		for (name, actual, reference, kind, freq) in [
			("low end", b.low - b.mid, curve.low, EqKind::LowShelf, 100.0),
			(
				"low-mids",
				b.low_mid - b.mid,
				curve.low_mid,
				EqKind::Bell,
				300.0,
			),
			(
				"high-mids",
				b.high_mid - b.mid,
				curve.high_mid,
				EqKind::Bell,
				3_500.0,
			),
			(
				"highs",
				b.high - b.mid,
				curve.high,
				EqKind::HighShelf,
				8_000.0,
			),
		] {
			let delta = actual - reference;
			if delta.abs() <= TONE_TOLERANCE_DB {
				continue;
			}
			let gain = if delta > 0.0 {
				-round_half((delta - 2.0).min(4.0))
			} else {
				round_half((-delta - 2.0).min(3.0))
			};
			push(
				if delta.abs() > 7.0 {
					Severity::Medium
				} else {
					Severity::Low
				},
				IssueKind::ToneBalance,
				None,
				format!(
					"{name} {:.1} dB {} a typical {} balance (vs the mids)",
					delta.abs(),
					if delta > 0.0 { "above" } else { "below" },
					targets.profile.name()
				),
				vec![MixMove::Eq {
					track_id: None,
					kind,
					freq_hz: freq,
					gain_db: gain,
					q: (kind == EqKind::Bell).then_some(1.0),
				}],
			);
		}
		if m.crest_factor_db < OVER_COMPRESSED_CREST_DB {
			let drive = master.limiter.map_or(0.0, |l| l.input_gain_db);
			push(
				Severity::Low,
				IssueKind::OverCompressed,
				None,
				format!(
					"master crest factor is {:.1} dB: over-compressed or over-limited, punch is \
					 lost",
					m.crest_factor_db
				),
				if drive > 3.0 {
					vec![MixMove::Limiter {
						track_id: None,
						ceiling_db: None,
						input_gain_db: Some(drive - 3.0),
					}]
				} else {
					Vec::new()
				},
			);
		}
	}

	for t in tracks.iter().filter(|t| !t.detail.is_silent()) {
		let peak = t.detail.stats.peak_db;
		if peak > HOT_TRACK_DB {
			push(
				Severity::Medium,
				IssueKind::TrackHot,
				Some(t.id),
				format!(
					"'{}' peaks at {peak:+.1} dBFS after its fader: no headroom; gain-stage it \
					 down",
					t.name
				),
				vec![MixMove::Gain {
					track_id: Some(t.id),
					delta_db: Some(-round_half(peak + 6.0)),
					db: None,
				}],
			);
		}
	}

	// Balance: each source's loudness against the level its role usually sits at, anchored
	// on the median so one outlier cannot move the reference.
	let sources: Vec<&TrackInput<'_>> = tracks
		.iter()
		.filter(|t| !t.is_bus && !t.detail.is_silent())
		.collect();
	if sources.len() > 1 {
		let mut anchors: Vec<f32> = sources
			.iter()
			.map(|t| t.detail.stats.lufs_integrated - targets.balance_db(t.role))
			.collect();
		anchors.sort_by(f32::total_cmp);
		let mid = anchors.len() / 2;
		let anchor = if anchors.len().is_multiple_of(2) {
			(anchors[mid - 1] + anchors[mid]) * 0.5
		} else {
			anchors[mid]
		};
		for t in &sources {
			let expected = anchor + targets.balance_db(t.role);
			let dev = t.detail.stats.lufs_integrated - expected;
			if dev.abs() > BALANCE_TOLERANCE_DB {
				push(
					Severity::Medium,
					IssueKind::Balance,
					Some(t.id),
					format!(
						"'{}' ({}) is {:.1} dB {} than its usual place in the balance",
						t.name,
						t.role.name(),
						dev.abs(),
						if dev > 0.0 { "louder" } else { "quieter" }
					),
					vec![MixMove::Gain {
						track_id: Some(t.id),
						delta_db: Some(-round_half(dev * 0.75)),
						db: None,
					}],
				);
			}
		}
	}

	for t in &sources {
		let Some(hp) = t.role.high_pass_hz() else {
			continue;
		};
		if t.high_pass_hz.is_some_and(|f| f >= hp * 0.5) {
			continue;
		}
		let o = &t.detail.spectrum.octave_db;
		let peak = o.iter().copied().fold(f32::MIN, f32::max);
		let lows = o[0].max(o[1]);
		if (lows > peak - 15.0 && hp > 100.0) || lows > peak - 10.0 {
			push(
				Severity::Low,
				IssueKind::Rumble,
				Some(t.id),
				format!(
					"'{}' ({}) carries energy below 80 Hz ({:.0} dB under its loudest band) \
					 that only muddies the lows: high-pass it at {}",
					t.name,
					t.role.name(),
					peak - lows,
					hz(hp)
				),
				vec![MixMove::Eq {
					track_id: Some(t.id),
					kind: EqKind::HighPass,
					freq_hz: hp,
					gain_db: 0.0,
					q: None,
				}],
			);
		}
	}

	for p in masking
		.iter()
		.filter(|p| p.score >= MASKING_ISSUE_SCORE)
		.take(MASKING_ISSUES)
	{
		let (a, b) = (&tracks[p.a], &tracks[p.b]);
		let (cut, keep) = if p.cut == p.a { (a, b) } else { (b, a) };
		let gain = if p.score > 0.5 { -4.0 } else { -3.0 };
		let low_pair = p.band_hz <= 125.0
			&& matches!(keep.role, Role::Kick | Role::Drums)
			&& cut.role == Role::Bass;
		push(
			if p.score > 0.5 {
				Severity::Medium
			} else {
				Severity::Low
			},
			IssueKind::Masking,
			Some(cut.id),
			format!(
				"'{}' and '{}' collide around {} {:.0}% of the time: cut '{}' {:.0} dB there{}",
				a.name,
				b.name,
				hz(p.band_hz),
				p.overlap * 100.0,
				cut.name,
				-gain,
				if low_pair {
					" (or duck it with a compressor keyed by the kick)"
				} else {
					""
				}
			),
			vec![MixMove::Eq {
				track_id: Some(cut.id),
				kind: EqKind::Bell,
				freq_hz: p.band_hz,
				gain_db: gain,
				q: Some(1.4),
			}],
		);
	}

	for w in warnings.iter().take(3) {
		push(
			Severity::Low,
			IssueKind::EngineWarning,
			None,
			w.clone(),
			Vec::new(),
		);
	}

	out.sort_by_key(|i| i.severity);
	for (n, issue) in out.iter_mut().enumerate() {
		issue.id = format!("i{}", n + 1);
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::spectrum::{Spectrum, Stereo, Timeline};
	use crate::{BandEnergy, LevelStats};

	fn detail(lufs: f32, peak: f32, octaves: [f32; 10]) -> Detail {
		Detail {
			stats: LevelStats {
				peak_db: peak,
				true_peak_db: peak,
				rms_db: lufs,
				lufs_integrated: lufs,
				lufs_short_term_max: lufs + 2.0,
				crest_factor_db: 12.0,
				clipped_samples: 0,
				band_energy_db: BandEnergy {
					low: -10.0,
					low_mid: -10.0,
					mid: -10.0,
					high_mid: -10.0,
					high: -10.0,
				},
				stereo_correlation: 0.6,
			},
			spectrum: Spectrum {
				third_octave_db: vec![-144.0; 31],
				side_third_octave_db: vec![-144.0; 31],
				octave_db: octaves.to_vec(),
			},
			stereo: Stereo {
				width: 0.15,
				low_correlation: 0.98,
				low_side_db: -30.0,
			},
			timeline: Timeline {
				hop_seconds: 0.085,
				frames: vec![octaves; 20],
			},
		}
	}

	/// A balanced master on the pop curve.
	fn pop_master(lufs: f32, true_peak: f32) -> Detail {
		let mut d = detail(lufs, true_peak, [-30.0; 10]);
		d.stats.band_energy_db = BandEnergy {
			low: -3.0,
			low_mid: -7.0,
			mid: -10.0,
			high_mid: -15.0,
			high: -18.0,
		};
		d
	}

	fn track<'a>(name: &'a str, role: Role, d: &'a Detail) -> TrackInput<'a> {
		TrackInput {
			id: TrackId::new(),
			name,
			role,
			is_bus: false,
			detail: d,
			pan: 0.0,
			high_pass_hz: None,
		}
	}

	const MID: [f32; 10] = [
		-144.0, -144.0, -40.0, -25.0, -20.0, -18.0, -22.0, -30.0, -40.0, -60.0,
	];

	#[test]
	fn a_clean_mix_on_target_has_no_issues() {
		let m = pop_master(-14.0, -1.5);
		let (a, b) = (detail(-16.0, -6.0, MID), detail(-17.0, -6.0, MID));
		let mut a = a;
		a.timeline.frames = vec![[-144.0; 10]; 20];
		let tracks = [
			track("Keys", Role::Keys, &a),
			track("Guitar", Role::Guitar, &b),
		];
		let issues = find_issues(
			&MasterInput {
				detail: &m,
				limiter: None,
			},
			&tracks,
			&MixTargets::default(),
			&track_masking(&tracks),
			&[],
		);
		assert!(issues.is_empty(), "{issues:#?}");
	}

	#[test]
	fn loudness_true_peak_and_balance_issues_carry_fixes() {
		let m = pop_master(-20.0, 0.4);
		let (drums, bass, lead) = (
			detail(-16.0, -3.0, MID),
			detail(-15.0, -3.0, MID),
			detail(-6.0, 0.2, MID),
		);
		let mut tracks = [
			track("Drums", Role::Drums, &drums),
			track("Bass", Role::Bass, &bass),
			track("Lead", Role::Lead, &lead),
		];
		for t in &mut tracks {
			t.high_pass_hz = Some(200.0);
		}
		let master = MasterInput {
			detail: &m,
			limiter: None,
		};
		let issues = find_issues(&master, &tracks, &MixTargets::default(), &[], &[]);
		let kinds: Vec<IssueKind> = issues.iter().map(|i| i.kind).collect();
		assert_eq!(issues[0].id, "i1");
		assert_eq!(issues[0].severity, Severity::High);
		assert!(kinds.contains(&IssueKind::TruePeak));
		let loud = issues
			.iter()
			.find(|i| i.kind == IssueKind::Loudness)
			.unwrap();
		assert!(loud.message.contains("6.0 LU quieter"), "{}", loud.message);
		// Too hot to just raise the fader: drive a limiter.
		assert!(matches!(
			loud.moves[0],
			MixMove::Limiter {
				input_gain_db: Some(g),
				ceiling_db: Some(c),
				..
			} if g == 6.0 && c == -1.5
		));
		let hot = issues
			.iter()
			.find(|i| i.kind == IssueKind::TrackHot)
			.unwrap();
		assert_eq!(hot.track_id, Some(tracks[2].id));
		let balance: Vec<&Issue> = issues
			.iter()
			.filter(|i| i.kind == IssueKind::Balance)
			.collect();
		assert_eq!(balance.len(), 1, "only the lead is out: {balance:?}");
		assert!(
			balance[0]
				.message
				.contains("'Lead' (lead) is 10.0 dB louder")
		);
		assert_eq!(
			balance[0].moves,
			vec![MixMove::Gain {
				track_id: Some(tracks[2].id),
				delta_db: Some(-7.5),
				db: None
			}]
		);

		// With a limiter the loudness fix drives it harder.
		let issues = find_issues(
			&MasterInput {
				detail: &m,
				limiter: Some(LimiterState {
					input_gain_db: 2.0,
					ceiling_db: -1.0,
				}),
			},
			&tracks,
			&MixTargets::default(),
			&[],
			&[],
		);
		let loud = issues
			.iter()
			.find(|i| i.kind == IssueKind::Loudness)
			.unwrap();
		assert!(matches!(
			loud.moves[0],
			MixMove::Limiter { input_gain_db: Some(g), .. } if g == 8.0
		));
	}

	#[test]
	fn bus_returns_and_silent_tracks_stay_out_of_the_balance() {
		let m = pop_master(-14.0, -1.5);
		let (a, b) = (detail(-16.0, -6.0, MID), detail(-16.5, -6.0, MID));
		let (verb, silent) = (detail(-45.0, -30.0, MID), detail(-144.0, -144.0, MID));
		let mut tracks = [
			track("Keys", Role::Keys, &a),
			track("Guitar", Role::Guitar, &b),
			track("Reverb", Role::Other, &verb),
			track("Unused", Role::Lead, &silent),
		];
		tracks[2].is_bus = true;
		let issues = find_issues(
			&MasterInput {
				detail: &m,
				limiter: None,
			},
			&tracks,
			&MixTargets::default(),
			&[],
			&[],
		);
		assert!(
			!issues.iter().any(|i| i.kind == IssueKind::Balance),
			"{issues:#?}"
		);
	}

	#[test]
	fn too_loud_backs_off_the_limiter_then_the_fader() {
		let m = pop_master(-6.0, -1.2);
		let master = MasterInput {
			detail: &m,
			limiter: Some(LimiterState {
				input_gain_db: 3.0,
				ceiling_db: -1.0,
			}),
		};
		let issues = find_issues(&master, &[], &MixTargets::default(), &[], &[]);
		let loud = issues
			.iter()
			.find(|i| i.kind == IssueKind::Loudness)
			.unwrap();
		assert_eq!(loud.moves.len(), 2);
		assert!(
			matches!(loud.moves[0], MixMove::Limiter { input_gain_db: Some(g), .. } if g == 0.0)
		);
		assert!(matches!(loud.moves[1], MixMove::Gain { delta_db: Some(d), .. } if d == -5.0));
	}

	#[test]
	fn tone_rumble_and_masking() {
		let mut m = pop_master(-14.0, -1.5);
		m.stats.band_energy_db.low = 6.0; // +9 over the mid band vs +7 typical... +16 total
		let mut pad_oct = MID;
		pad_oct[0] = -22.0;
		pad_oct[1] = -20.0;
		let pad = detail(-24.0, -10.0, pad_oct);
		let kick_oct = [
			-144.0, -12.0, -20.0, -40.0, -50.0, -60.0, -40.0, -45.0, -60.0, -80.0,
		];
		let bass_oct = [
			-144.0, -14.0, -12.0, -25.0, -40.0, -60.0, -70.0, -80.0, -90.0, -100.0,
		];
		let (kick, bass) = (detail(-12.0, -3.0, kick_oct), detail(-13.0, -3.0, bass_oct));
		let tracks = [
			track("Kick", Role::Kick, &kick),
			track("Bass", Role::Bass, &bass),
			track("Pad", Role::Pad, &pad),
		];
		let masking = track_masking(&tracks);
		let issues = find_issues(
			&MasterInput {
				detail: &m,
				limiter: None,
			},
			&tracks,
			&MixTargets::default(),
			&masking,
			&[],
		);
		let tone = issues
			.iter()
			.find(|i| i.kind == IssueKind::ToneBalance)
			.unwrap();
		assert!(
			tone.message.starts_with("low end 9.0 dB above"),
			"{}",
			tone.message
		);
		assert!(matches!(
			tone.moves[0],
			MixMove::Eq { kind: EqKind::LowShelf, gain_db, .. } if gain_db == -4.0
		));
		let rumble = issues.iter().find(|i| i.kind == IssueKind::Rumble).unwrap();
		assert_eq!(rumble.track_id, Some(tracks[2].id));
		let mask = issues
			.iter()
			.find(|i| i.kind == IssueKind::Masking)
			.unwrap();
		assert_eq!(mask.track_id, Some(tracks[1].id), "{}", mask.message);
		assert!(
			mask.message.contains("keyed by the kick"),
			"{}",
			mask.message
		);
	}
}
