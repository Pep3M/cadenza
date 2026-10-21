//! Pure logic of the Electric Piano editor (`electric_piano`, design `17 Instruments`,
//! 17b): the stylised partial spectrum of a held C4 (brightness tilts it, bell lifts
//! the 7th–9th partials), the Mellow ↔ Bright slider, and one note over three seconds
//! (the processor's attack / exponential decay / release, held for two seconds) under
//! the stereo tremolo, both channels as the processor computes them.

use cadenza_dsp::{DeviceInfo, ParamInfo};

pub const BRIGHTNESS: &str = "brightness";
pub const BELL: &str = "bell";
pub const DECAY: &str = "decay";
pub const RELEASE: &str = "release";
pub const VEL_SENS: &str = "velocity_sensitivity";
pub const TREM_RATE: &str = "tremolo_rate";
pub const TREM_DEPTH: &str = "tremolo_depth";
pub const GAIN: &str = "gain";

/// Bars of the tone picture.
pub const PARTIALS: usize = 20;
/// The bell lifts these partials (1-based).
pub const BELL_PARTIALS: std::ops::RangeInclusive<usize> = 7..=9;
/// The note picture spans this long, the key goes up at [`NOTE_HOLD`].
pub const NOTE_SPAN: f32 = 3.0;
pub const NOTE_HOLD: f32 = 2.0;
/// The processor's attack (ms).
const ATTACK_MS: f32 = 2.0;
/// `decay_coeff`: an exponential stage reaches -60 dB at its time.
const LN_1000: f32 = 6.907_755;

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("electric_piano").expect("the electric piano is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("an electric piano param")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpValues {
	pub brightness: f32,
	pub bell: f32,
	pub decay: f32,
	pub release: f32,
	pub trem_rate: f32,
	pub trem_depth: f32,
}

impl Default for EpValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl EpValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			brightness: f(param(BRIGHTNESS)),
			bell: f(param(BELL)),
			decay: f(param(DECAY)),
			release: f(param(RELEASE)),
			trem_rate: f(param(TREM_RATE)),
			trem_depth: f(param(TREM_DEPTH)),
		}
	}
}

/// One bar of the tone picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Partial {
	/// Height, 0..1 (at least a sliver).
	pub level: f32,
	/// One of the bell partials (drawn pale).
	pub bell: bool,
}

/// The partials of a held C4: the FM index (brightness) flattens the roll-off above the
/// fundamental, the bell lifts the 7th–9th. Stylised (70 dB on the bar height) but
/// monotonic in both params.
pub fn partials(brightness: f32, bell: f32) -> [Partial; PARTIALS] {
	let br = (brightness / 100.0).clamp(0.0, 1.0);
	let bell_amt = (bell / 100.0).clamp(0.0, 1.0);
	std::array::from_fn(|i| {
		let k = i + 1;
		let is_bell = BELL_PARTIALS.contains(&k);
		let mut db = -((k - 1) as f32) * (6.0 - 4.0 * br) - if k > 1 { 4.0 } else { 0.0 };
		if is_bell {
			db += bell_amt * 22.0;
		}
		Partial {
			level: (1.0 + db / 70.0).clamp(0.03, 1.0),
			bell: is_bell,
		}
	})
}

/// Brightness at fraction `x` (0 = Mellow, 1 = Bright) of the slider.
pub fn slider_brightness(x: f32) -> f32 {
	let p = param(BRIGHTNESS);
	(p.min + x.clamp(0.0, 1.0) * (p.max - p.min)).round()
}

/// Where `brightness` sits on the slider (0..1).
pub fn slider_pos(brightness: f32) -> f32 {
	let p = param(BRIGHTNESS);
	((brightness - p.min) / (p.max - p.min)).clamp(0.0, 1.0)
}

/// The amplitude of a C4 struck at 0 and released at [`NOTE_HOLD`], `t` seconds in:
/// the processor's linear attack and exponential decay (to -60 dB at `decay`) and
/// release.
pub fn note_envelope(v: &EpValues, t: f32) -> f32 {
	let held = |t: f32| {
		let attack = ATTACK_MS * 0.001;
		if t < attack {
			t / attack
		} else {
			(-LN_1000 * (t - attack) / (v.decay.max(1.0) * 0.001)).exp()
		}
	};
	if t < 0.0 {
		0.0
	} else if t <= NOTE_HOLD {
		held(t)
	} else {
		held(NOTE_HOLD) * (-LN_1000 * (t - NOTE_HOLD) / (v.release.max(1.0) * 0.001)).exp()
	}
}

/// Where a sounding voice sits on the note picture (seconds of its axis): held voices
/// run up to the key-up line and wait there, released ones continue after it.
pub fn voice_time(released: bool, elapsed_ms: f32) -> f32 {
	let t = elapsed_ms.max(0.0) * 0.001;
	if released {
		(NOTE_HOLD + t).min(NOTE_SPAN)
	} else {
		t.min(NOTE_HOLD)
	}
}

/// The tremolo's gain on the left and right channel `t` seconds in (the processor's
/// auto-pan: `1 - depth * (0.5 ± 0.5 sin)`), starting at phase 0.
pub fn tremolo_gains(v: &EpValues, t: f32) -> [f32; 2] {
	let depth = (v.trem_depth / 100.0).clamp(0.0, 1.0);
	let s = (std::f32::consts::TAU * v.trem_rate * t).sin();
	[1.0 - depth * (0.5 + 0.5 * s), 1.0 - depth * (0.5 - 0.5 * s)]
}

/// "4.5 Hz · 40 %" (the tremolo display's corner).
pub fn tremolo_label(v: &EpValues) -> String {
	let rate = if v.trem_rate < 10.0 {
		format!("{:.1} Hz", v.trem_rate)
	} else {
		format!("{:.0} Hz", v.trem_rate)
	};
	if v.trem_depth < 0.5 {
		format!("{rate} \u{b7} off")
	} else {
		format!("{rate} \u{b7} {:.0} %", v.trem_depth)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn voices_ride_the_note_picture() {
		assert_eq!(voice_time(false, 500.0), 0.5);
		assert_eq!(voice_time(false, 9000.0), NOTE_HOLD);
		assert_eq!(voice_time(true, 250.0), NOTE_HOLD + 0.25);
		assert_eq!(voice_time(true, 1e6), NOTE_SPAN);
	}

	#[test]
	fn brightness_flattens_the_roll_off_and_bell_lifts_only_its_partials() {
		let dull = partials(10.0, 0.0);
		let bright = partials(90.0, 0.0);
		assert_eq!(dull[0].level, bright[0].level, "the fundamental stays");
		for k in 1..PARTIALS {
			assert!(bright[k].level >= dull[k].level, "partial {}", k + 1);
		}
		assert!(bright[4].level > dull[4].level);
		for spectrum in [dull, bright] {
			for k in 1..PARTIALS {
				if !BELL_PARTIALS.contains(&(k + 1)) && !BELL_PARTIALS.contains(&k) {
					assert!(spectrum[k].level <= spectrum[k - 1].level);
				}
			}
		}
		let no_bell = partials(50.0, 0.0);
		let bell = partials(50.0, 80.0);
		for k in 0..PARTIALS {
			if BELL_PARTIALS.contains(&(k + 1)) {
				assert!(bell[k].bell);
				assert!(bell[k].level > no_bell[k].level);
			} else {
				assert!(!bell[k].bell);
				assert_eq!(bell[k].level, no_bell[k].level);
			}
		}
	}

	#[test]
	fn the_slider_round_trips_brightness() {
		assert_eq!(slider_brightness(0.0), 0.0);
		assert_eq!(slider_brightness(1.0), 100.0);
		assert_eq!(slider_brightness(slider_pos(38.0)), 38.0);
		assert_eq!(slider_brightness(1.4), 100.0);
	}

	#[test]
	fn the_note_decays_with_decay_and_falls_after_the_key_goes_up() {
		let short = EpValues {
			decay: 1_000.0,
			..EpValues::default()
		};
		let long = EpValues {
			decay: 8_000.0,
			..EpValues::default()
		};
		assert!(note_envelope(&long, 0.0) < 0.01, "attack starts at silence");
		assert!((note_envelope(&long, 0.002) - 1.0).abs() < 1e-3);
		assert!(note_envelope(&long, 1.0) > note_envelope(&short, 1.0));
		// -60 dB after `decay`.
		assert!((note_envelope(&short, 1.002) - 0.001).abs() < 1e-4);
		let slow = EpValues {
			release: 2_000.0,
			..long
		};
		let fast = EpValues {
			release: 50.0,
			..long
		};
		assert_eq!(
			note_envelope(&slow, NOTE_HOLD),
			note_envelope(&fast, NOTE_HOLD)
		);
		assert!(note_envelope(&fast, NOTE_HOLD + 0.2) < 0.01);
		assert!(note_envelope(&slow, NOTE_HOLD + 0.2) > note_envelope(&fast, NOTE_HOLD + 0.2));
	}

	#[test]
	fn the_tremolo_pans_the_channels_against_each_other() {
		let off = EpValues {
			trem_depth: 0.0,
			..EpValues::default()
		};
		assert_eq!(tremolo_gains(&off, 0.37), [1.0, 1.0]);
		let on = EpValues {
			trem_depth: 40.0,
			trem_rate: 4.5,
			..EpValues::default()
		};
		// A quarter cycle in: left at its lowest, right untouched.
		let [l, r] = tremolo_gains(&on, 0.25 / 4.5);
		assert!((l - 0.6).abs() < 1e-4 && (r - 1.0).abs() < 1e-4);
		assert_eq!(tremolo_label(&on), "4.5 Hz \u{b7} 40 %");
		assert_eq!(tremolo_label(&off), "4.5 Hz \u{b7} off");
	}
}
