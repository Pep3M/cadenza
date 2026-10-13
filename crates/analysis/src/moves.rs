//! Mix moves: the small vocabulary of mixing actions an issue proposes as its fix and the
//! AI (or the app's "fix" buttons) applies in one batch. They name tracks by id and the
//! built-in devices by role; turning them into project commands is the MCP layer's job.

use cadenza_core::TrackId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Filter type of an [`MixMove::Eq`] band, mapped onto the built-in Parametric EQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EqKind {
	/// Peak band (one of the EQ's three): cut or boost around `freq_hz`.
	Bell,
	LowShelf,
	HighShelf,
	/// Removes everything below `freq_hz` (12 dB/oct; `gain_db`/`q` unused).
	HighPass,
	/// Removes everything above `freq_hz`.
	LowPass,
}

/// One mixing action. Omitting `track_id` targets the master channel where allowed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum MixMove {
	/// Fader: `delta_db` relative to the current volume, or `db` absolute (one of them).
	Gain {
		#[serde(default, skip_serializing_if = "Option::is_none")]
		track_id: Option<TrackId>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		delta_db: Option<f32>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		db: Option<f32>,
	},
	/// Pan position -1 (left) .. 1 (right).
	Pan { track_id: TrackId, pan: f32 },
	/// One band of the channel's built-in Parametric EQ (inserted first in the chain when
	/// the channel has none; a bell reuses the band nearest in frequency or a free one).
	Eq {
		#[serde(default, skip_serializing_if = "Option::is_none")]
		track_id: Option<TrackId>,
		kind: EqKind,
		freq_hz: f32,
		/// Bells and shelves; cuts of 2-4 dB are usually enough.
		#[serde(default)]
		gain_db: f32,
		/// Bells only; default 1.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		q: Option<f32>,
	},
	/// Settings of the channel's built-in compressor (inserted when missing); unset fields
	/// keep their value.
	Compressor {
		#[serde(default, skip_serializing_if = "Option::is_none")]
		track_id: Option<TrackId>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		threshold_db: Option<f32>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		ratio: Option<f32>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		attack_ms: Option<f32>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		release_ms: Option<f32>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		makeup_db: Option<f32>,
	},
	/// Settings of the channel's built-in limiter (inserted last in the chain when missing;
	/// default: the master).
	Limiter {
		#[serde(default, skip_serializing_if = "Option::is_none")]
		track_id: Option<TrackId>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		ceiling_db: Option<f32>,
		/// Drive into the limiter (0..24 dB): raises loudness.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		input_gain_db: Option<f32>,
	},
	/// Send level from a track to a bus (adds the send when missing).
	Send {
		track_id: TrackId,
		bus_id: TrackId,
		level_db: f32,
	},
}

impl MixMove {
	/// The channel the move edits (`None` = master).
	pub fn track(&self) -> Option<TrackId> {
		match self {
			MixMove::Gain { track_id, .. }
			| MixMove::Eq { track_id, .. }
			| MixMove::Compressor { track_id, .. }
			| MixMove::Limiter { track_id, .. } => *track_id,
			MixMove::Pan { track_id, .. } | MixMove::Send { track_id, .. } => Some(*track_id),
		}
	}

	/// A short human label ("Bass: bell -3 dB at 250 Hz"); `name` resolves track ids.
	pub fn describe(&self, name: &dyn Fn(Option<TrackId>) -> String) -> String {
		let who = name(self.track());
		match self {
			MixMove::Gain { delta_db, db, .. } => match (delta_db, db) {
				(Some(d), _) => format!("{who}: fader {d:+.1} dB"),
				(None, Some(v)) => format!("{who}: fader to {v:.1} dB"),
				(None, None) => format!("{who}: fader unchanged"),
			},
			MixMove::Pan { pan, .. } => format!("{who}: pan {}", pan_label(*pan)),
			MixMove::Eq {
				kind,
				freq_hz,
				gain_db,
				..
			} => match kind {
				EqKind::HighPass => format!("{who}: high-pass at {}", hz(*freq_hz)),
				EqKind::LowPass => format!("{who}: low-pass at {}", hz(*freq_hz)),
				EqKind::Bell => format!("{who}: EQ {gain_db:+.1} dB at {}", hz(*freq_hz)),
				EqKind::LowShelf => {
					format!("{who}: low shelf {gain_db:+.1} dB below {}", hz(*freq_hz))
				}
				EqKind::HighShelf => {
					format!("{who}: high shelf {gain_db:+.1} dB above {}", hz(*freq_hz))
				}
			},
			MixMove::Compressor {
				threshold_db,
				ratio,
				..
			} => {
				let mut s = format!("{who}: compressor");
				if let Some(t) = threshold_db {
					s.push_str(&format!(" {t:.0} dB"));
				}
				if let Some(r) = ratio {
					s.push_str(&format!(" {r:.1}:1"));
				}
				s
			}
			MixMove::Limiter {
				ceiling_db,
				input_gain_db,
				..
			} => {
				let mut s = format!("{who}: limiter");
				if let Some(g) = input_gain_db {
					s.push_str(&format!(" drive {g:.1} dB"));
				}
				if let Some(c) = ceiling_db {
					s.push_str(&format!(" ceiling {c:.1} dB"));
				}
				s
			}
			MixMove::Send {
				bus_id, level_db, ..
			} => format!("{who}: send to {} at {level_db:.1} dB", name(Some(*bus_id))),
		}
	}
}

pub fn hz(freq: f32) -> String {
	if freq >= 1_000.0 {
		format!("{:.1} kHz", freq / 1_000.0)
	} else {
		format!("{freq:.0} Hz")
	}
}

pub fn pan_label(pan: f32) -> String {
	if pan.abs() < 0.005 {
		"center".into()
	} else if pan < 0.0 {
		format!("{:.0}% left", -pan * 100.0)
	} else {
		format!("{:.0}% right", pan * 100.0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn moves_round_trip_through_json_and_describe_themselves() {
		let bass = TrackId::new();
		let moves = vec![
			MixMove::Gain {
				track_id: None,
				delta_db: Some(-2.0),
				db: None,
			},
			MixMove::Eq {
				track_id: Some(bass),
				kind: EqKind::Bell,
				freq_hz: 250.0,
				gain_db: -3.0,
				q: Some(1.4),
			},
			MixMove::Pan {
				track_id: bass,
				pan: -0.3,
			},
		];
		let json = serde_json::to_string(&moves).unwrap();
		assert!(json.contains("\"move\":\"eq\"") && json.contains("\"kind\":\"bell\""));
		let back: Vec<MixMove> = serde_json::from_str(&json).unwrap();
		assert_eq!(back, moves);
		let name = |t: Option<TrackId>| t.map_or("Master".to_owned(), |_| "Bass".to_owned());
		assert_eq!(moves[0].describe(&name), "Master: fader -2.0 dB");
		assert_eq!(moves[1].describe(&name), "Bass: EQ -3.0 dB at 250 Hz");
		assert_eq!(moves[2].describe(&name), "Bass: pan 30% left");
		let parsed: MixMove =
			serde_json::from_str(r#"{"move":"eq","kind":"high_pass","freq_hz":120}"#).unwrap();
		assert_eq!(parsed.describe(&name), "Master: high-pass at 120 Hz");
	}
}
