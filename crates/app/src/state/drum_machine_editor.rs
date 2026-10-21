//! Pure logic of the Drum Machine editor (`drum_machine`, design `17 Instruments`,
//! 17c): the seven voices (their GM notes, params, colour slot), each voice's amplitude
//! envelope and the kick's pitch sweep from the processor's own formulas, the level bar
//! (drag ⇔), voice navigation, and pad flashes from the processor's trigger counters
//! ([`cadenza_dsp::readings::DRUM_VOICE_TRIGGERS_START`]).

use crate::i18n::mark;
use cadenza_dsp::readings;
use cadenza_dsp::{DeviceInfo, ParamInfo};

pub const TONE: &str = "tone";
pub const GAIN: &str = "gain";

/// A strip flashes this long after its voice was triggered (seconds).
pub const FLASH_SECONDS: f64 = 0.15;
/// The amplitude pictures show `amplitude ^ DISPLAY_POWER` so tails stay visible.
const DISPLAY_POWER: f32 = 0.5;
/// `decay_coeff`: an exponential stage reaches -60 dB at its time.
const LN_1000: f32 = 6.907_755;

/// One knob of a voice: param id and its short label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceParam {
	pub id: &'static str,
	pub label: &'static str,
}

const fn vp(id: &'static str, label: &'static str) -> VoiceParam {
	VoiceParam { id, label }
}

/// A voice of the kit as the editor shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Voice {
	pub name: &'static str,
	/// For narrow pads.
	pub short: &'static str,
	/// "C1 · 36" (the processor's description names notes with C1 = 36).
	pub note_label: &'static str,
	/// The note a click on its pad plays.
	pub audition: u8,
	/// Index into the kit's 8-colour palette.
	pub color: usize,
	/// Every knob, the level last.
	pub params: &'static [VoiceParam],
	/// The two shown as text on the strip.
	pub summary: &'static [VoiceParam],
	pub level: &'static str,
	/// The span of its pictures (ms).
	pub span_ms: f32,
}

/// Kick, snare, clap, hats, toms, cymbals, percussion: the order of the processor's
/// trigger counters.
pub const VOICES: [Voice; readings::DRUM_VOICES] = [
	Voice {
		name: mark("Kick"),
		short: mark("Kick"),
		note_label: "C1 \u{b7} 36",
		audition: 36,
		color: 0,
		params: &[
			vp("kick_tune", mark("Tune")),
			vp("kick_decay", mark("Decay")),
			vp("kick_punch", mark("Punch")),
			vp("kick_level", mark("Level")),
		],
		summary: &[
			vp("kick_tune", mark("Tune")),
			vp("kick_punch", mark("Punch")),
		],
		level: "kick_level",
		span_ms: 600.0,
	},
	Voice {
		name: mark("Snare"),
		short: mark("Snare"),
		note_label: "D1 \u{b7} 38",
		audition: 38,
		color: 1,
		params: &[
			vp("snare_tune", mark("Tune")),
			vp("snare_decay", mark("Decay")),
			vp("snare_snappy", mark("Snappy")),
			vp("snare_level", mark("Level")),
		],
		summary: &[
			vp("snare_tune", mark("Tune")),
			vp("snare_snappy", mark("Snappy")),
		],
		level: "snare_level",
		span_ms: 400.0,
	},
	Voice {
		name: mark("Clap"),
		short: mark("Clap"),
		note_label: "D#1 \u{b7} 39",
		audition: 39,
		color: 2,
		params: &[vp("clap_level", mark("Level"))],
		summary: &[vp("clap_level", mark("Level"))],
		level: "clap_level",
		span_ms: 400.0,
	},
	Voice {
		name: mark("Hats"),
		short: mark("Hats"),
		note_label: "F#1 \u{b7} 42/46",
		audition: 42,
		color: 3,
		params: &[
			vp("hat_tune", mark("Tune")),
			vp("hat_decay", mark("Closed decay")),
			vp("open_hat_decay", mark("Open decay")),
			vp("hat_level", mark("Level")),
		],
		summary: &[
			vp("hat_decay", mark("Closed")),
			vp("open_hat_decay", mark("Open")),
		],
		level: "hat_level",
		span_ms: 600.0,
	},
	Voice {
		name: mark("Toms"),
		short: mark("Toms"),
		note_label: "A1 \u{b7} 41\u{2013}50",
		audition: 45,
		color: 4,
		params: &[
			vp("tom_tune", mark("Tune")),
			vp("tom_decay", mark("Decay")),
			vp("tom_level", mark("Level")),
		],
		summary: &[vp("tom_tune", mark("Tune")), vp("tom_decay", mark("Decay"))],
		level: "tom_level",
		span_ms: 600.0,
	},
	Voice {
		name: mark("Cymbals"),
		short: mark("Cymbals"),
		note_label: "C#2 \u{b7} 49\u{2013}59",
		audition: 49,
		color: 6,
		params: &[
			vp("cymbal_decay", mark("Decay")),
			vp("cymbal_level", mark("Level")),
		],
		summary: &[vp("cymbal_decay", mark("Decay"))],
		level: "cymbal_level",
		span_ms: 2_000.0,
	},
	Voice {
		name: mark("Percussion"),
		short: mark("Perc"),
		note_label: "C3 \u{b7} 60\u{2013}81",
		audition: 60,
		color: 7,
		params: &[
			vp("perc_tune", mark("Tune")),
			vp("perc_decay", mark("Decay")),
			vp("perc_level", mark("Level")),
		],
		summary: &[
			vp("perc_tune", mark("Tune")),
			vp("perc_decay", mark("Decay")),
		],
		level: "perc_level",
		span_ms: 300.0,
	},
];

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("drum_machine").expect("the drum machine is a built-in")
}

pub fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a drum machine param")
}

/// The voice left (`-1`) or right (`+1`) of `voice`, staying at the ends.
pub fn step_voice(voice: usize, delta: i32) -> usize {
	(voice as i32 + delta).clamp(0, VOICES.len() as i32 - 1) as usize
}

/// How full the level bar is for `db` (0..1, linear in dB over the param's range).
pub fn level_fraction(id: &str, db: f32) -> f32 {
	let p = param(id);
	((db - p.min) / (p.max - p.min)).clamp(0.0, 1.0)
}

/// The level after dragging `dx` px across a bar `width` px wide, from `start` dB
/// (the bar spans the range); rounded to 0.1 dB.
pub fn drag_level(id: &str, start: f32, dx: f32, width: f32) -> f32 {
	let p = param(id);
	let v = start + dx / width.max(1.0) * (p.max - p.min);
	((v * 10.0).round() / 10.0).clamp(p.min, p.max)
}

/// Every param of `voice` back to its default (the pad's double-click).
pub fn reset_voice(voice: &Voice) -> Vec<(&'static str, f32)> {
	voice
		.params
		.iter()
		.map(|p| (p.id, param(p.id).default))
		.collect()
}

fn decay(ms: f32, t_ms: f32) -> f32 {
	(-LN_1000 * t_ms / ms.max(1.0)).exp()
}

fn semis(s: f32) -> f32 {
	2.0_f32.powf(s / 12.0)
}

/// The voice's amplitude `t_ms` after a hit (0..1, not yet display-shaped), from its
/// params `get` as the processor builds the sound: kick body, snare tone + wires by
/// snappiness, the clap's three bursts and tail, the closed hat (`open` = the open
/// one), toms, crash, and the hand percussion's typical length.
pub fn amplitude(voice: usize, get: &dyn Fn(&str) -> f32, t_ms: f32, open: bool) -> f32 {
	if t_ms < 0.0 {
		return 0.0;
	}
	match voice {
		0 => decay(get("kick_decay"), t_ms),
		1 => {
			let d = get("snare_decay");
			let s = get("snare_snappy") / 100.0;
			let (tone, wires) = (1.0 - 0.6 * s, 0.3 + 0.9 * s);
			(tone * decay(d * 0.55, t_ms) + wires * decay(d, t_ms)) / (tone + wires)
		}
		2 => {
			// Three 11 ms bursts, each falling over 9 ms, then a 220 ms tail.
			let burst = 11.0;
			let mut a = 0.0;
			if t_ms < burst * 3.0 {
				a += decay(9.0, t_ms % burst);
			}
			if t_ms >= burst * 2.0 {
				a += 0.8 * decay(220.0, t_ms - burst * 2.0);
			}
			a.min(1.0)
		}
		3 if open => decay(get("open_hat_decay"), t_ms),
		3 => decay(get("hat_decay"), t_ms),
		4 => decay(get("tom_decay"), t_ms),
		5 => decay(get("cymbal_decay"), t_ms),
		_ => decay(150.0 * get("perc_decay") / 100.0, t_ms),
	}
}

/// [`amplitude`] as the pictures draw it (tails lifted so a long decay reads longer).
pub fn display_amplitude(voice: usize, get: &dyn Fn(&str) -> f32, t_ms: f32, open: bool) -> f32 {
	amplitude(voice, get, t_ms, open).powf(DISPLAY_POWER)
}

/// The kick's frequency `t_ms` after a hit (Hz): the body tuned by `kick_tune`, swept
/// down from up to 6× by `kick_punch`.
pub fn kick_frequency(get: &dyn Fn(&str) -> f32, t_ms: f32) -> f32 {
	let punch = get("kick_punch") / 100.0;
	let base = 50.0 * semis(get("kick_tune"));
	let sweep = 1.0 + 5.0 * punch;
	let pe = decay(35.0 + 25.0 * (1.0 - punch), t_ms);
	base * (1.0 + (sweep - 1.0) * pe * pe)
}

/// The highest frequency [`kick_frequency`] reaches at any setting (picture scale).
pub fn kick_frequency_max() -> f32 {
	50.0 * semis(12.0) * 6.0
}

/// A strip's short value text: whole milliseconds from 10 ms to a second ("45 ms"), else the
/// usual readout.
pub fn strip_value(value: f32, unit: &str) -> String {
	if unit == "ms" && (10.0..1_000.0).contains(&value) {
		format!("{value:.0} ms")
	} else {
		crate::state::device::format_value(value, unit)
	}
}

/// "600 ms" / "2.0 s" (the span tag of a picture).
pub fn span_label(ms: f32) -> String {
	if ms < 1_000.0 {
		format!("{ms:.0} ms")
	} else {
		format!("{:.1} s", ms / 1_000.0)
	}
}

/// Pad flashes from the processor's trigger counters. The first readings only set the
/// baseline; a counter that went down (a fresh instance after a reload) rebases
/// without flashing.
#[derive(Debug, Clone, Default)]
pub struct Flashes {
	last: Option<[f32; readings::DRUM_VOICES]>,
	/// When each voice last flashed (input time).
	pub at: [Option<f64>; readings::DRUM_VOICES],
}

impl Flashes {
	/// Takes the counters at `now`; returns the voices that were triggered since.
	pub fn update(&mut self, counters: &[f32], now: f64) -> [bool; readings::DRUM_VOICES] {
		let mut hit = [false; readings::DRUM_VOICES];
		if counters.len() < readings::DRUM_VOICES {
			return hit;
		}
		let start = readings::DRUM_VOICE_TRIGGERS_START;
		let mut current = [0.0; readings::DRUM_VOICES];
		current.copy_from_slice(&counters[start..start + readings::DRUM_VOICES]);
		if let Some(last) = self.last {
			for i in 0..readings::DRUM_VOICES {
				if current[i] > last[i] {
					hit[i] = true;
					self.at[i] = Some(now);
				}
			}
		}
		self.last = Some(current);
		hit
	}

	/// How lit voice `i` is at `now` (1 at the trigger, 0 after [`FLASH_SECONDS`]).
	pub fn level(&self, i: usize, now: f64) -> f32 {
		self.at
			.get(i)
			.copied()
			.flatten()
			.map_or(0.0, |t| flash_level(now - t))
	}

	pub fn any_lit(&self, now: f64) -> bool {
		(0..readings::DRUM_VOICES).any(|i| self.level(i, now) > 0.0)
	}
}

/// The flash's brightness `elapsed` seconds after a trigger (quadratic fall).
pub fn flash_level(elapsed: f64) -> f32 {
	if !(0.0..FLASH_SECONDS).contains(&elapsed) {
		return 0.0;
	}
	let x = 1.0 - (elapsed / FLASH_SECONDS) as f32;
	x * x
}

#[cfg(test)]
mod tests {
	use super::*;

	fn defaults(id: &str) -> f32 {
		param(id).default
	}

	#[test]
	fn every_voice_names_real_params_and_auditions_a_note_the_kit_plays() {
		for v in VOICES {
			for p in v.params.iter().chain(v.summary) {
				assert!(info().param(p.id).is_some(), "{}", p.id);
			}
			assert_eq!(v.params.last().unwrap().id, v.level);
			assert!(cadenza_dsp::instruments::drum_machine::plays(v.audition));
		}
		assert_eq!(VOICES.len(), readings::DRUM_VOICE_NAMES.len());
		for (v, name) in VOICES.iter().zip(readings::DRUM_VOICE_NAMES) {
			assert!(v.name.to_lowercase().starts_with(&name[..3]), "{name}");
		}
	}

	#[test]
	fn longer_decays_ring_longer_and_snappy_moves_the_snare() {
		let short = |id: &str| {
			if id == "kick_decay" {
				200.0
			} else {
				defaults(id)
			}
		};
		let long = |id: &str| {
			if id == "kick_decay" {
				1_500.0
			} else {
				defaults(id)
			}
		};
		assert_eq!(amplitude(0, &short, 0.0, false), 1.0);
		assert!(amplitude(0, &long, 150.0, false) > amplitude(0, &short, 150.0, false));
		assert!((amplitude(0, &short, 200.0, false) - 0.001).abs() < 1e-4);
		// The open hat outlasts the closed one at the defaults.
		assert!(amplitude(3, &defaults, 120.0, true) > amplitude(3, &defaults, 120.0, false));
		// Wires (the long part) take over with snappiness.
		let dry = |id: &str| {
			if id == "snare_snappy" {
				0.0
			} else {
				defaults(id)
			}
		};
		let snappy = |id: &str| {
			if id == "snare_snappy" {
				100.0
			} else {
				defaults(id)
			}
		};
		assert!(amplitude(1, &snappy, 150.0, false) > amplitude(1, &dry, 150.0, false));
		// The clap's second burst re-strikes.
		assert!(amplitude(2, &defaults, 11.5, false) > amplitude(2, &defaults, 10.5, false));
		assert!(
			display_amplitude(4, &defaults, 300.0, false) > amplitude(4, &defaults, 300.0, false)
		);
	}

	#[test]
	fn punch_deepens_the_kick_sweep_and_tune_moves_the_body() {
		let soft = |id: &str| {
			if id == "kick_punch" {
				0.0
			} else {
				defaults(id)
			}
		};
		let hard = |id: &str| {
			if id == "kick_punch" {
				100.0
			} else {
				defaults(id)
			}
		};
		assert!((kick_frequency(&soft, 0.0) - 50.0).abs() < 1e-3);
		assert!((kick_frequency(&hard, 0.0) - 300.0).abs() < 1e-2);
		assert!((kick_frequency(&hard, 2_000.0) - 50.0).abs() < 0.1);
		let up = |id: &str| {
			if id == "kick_tune" {
				12.0
			} else {
				defaults(id)
			}
		};
		assert!((kick_frequency(&up, 2_000.0) - 100.0).abs() < 0.2);
		assert!(kick_frequency(&hard, 0.0) <= kick_frequency_max());
	}

	#[test]
	fn the_level_bar_spans_the_range_and_voices_step_inside_the_kit() {
		assert_eq!(level_fraction("kick_level", -60.0), 0.0);
		assert_eq!(level_fraction("kick_level", 6.0), 1.0);
		assert_eq!(drag_level("kick_level", 0.0, 50.0, 100.0), 6.0);
		assert_eq!(drag_level("kick_level", 0.0, -10.0, 110.0), -6.0);
		assert_eq!(step_voice(0, -1), 0);
		assert_eq!(step_voice(0, 1), 1);
		assert_eq!(step_voice(6, 1), 6);
		let reset = reset_voice(&VOICES[0]);
		assert_eq!(reset.len(), 4);
		assert!(reset.contains(&("kick_decay", 450.0)));
		assert_eq!(span_label(600.0), "600 ms");
		assert_eq!(strip_value(45.0, "ms"), "45 ms");
		assert_eq!(
			strip_value(-2.0, "semitones"),
			crate::state::device::format_value(-2.0, "semitones")
		);
		assert_eq!(span_label(2_000.0), "2.0 s");
	}

	#[test]
	fn counters_that_grow_flash_their_strip_for_a_moment() {
		let mut f = Flashes::default();
		let mut c = [0.0; readings::MAX];
		c[readings::DRUM_KICK] = 4.0;
		assert_eq!(f.update(&c[..7], 1.0), [false; 7], "the baseline");
		c[readings::DRUM_KICK] = 5.0;
		c[readings::DRUM_HATS] = 1.0;
		let hit = f.update(&c[..7], 2.0);
		assert!(hit[readings::DRUM_KICK] && hit[readings::DRUM_HATS]);
		assert!(!hit[readings::DRUM_SNARE]);
		assert_eq!(f.level(readings::DRUM_KICK, 2.0), 1.0);
		assert!(f.level(readings::DRUM_KICK, 2.1) > 0.0);
		assert_eq!(f.level(readings::DRUM_KICK, 2.0 + FLASH_SECONDS), 0.0);
		assert!(!f.any_lit(3.0));
		// A reload starts the counters over: no flash.
		let fresh = [0.0; 7];
		assert_eq!(f.update(&fresh, 4.0), [false; 7]);
		assert_eq!(f.update(&c[..3], 5.0), [false; 7], "too few readings");
	}
}
