//! Pure logic of the Pitch Correct editor (`pitch_correct`): the device's values, which
//! notes its key and scale keep, the live pitch trace read from the probe (detected and
//! corrected pitch, target note) with the semitone range it is drawn over, and the
//! readouts.

use std::collections::VecDeque;

use cadenza_dsp::effects::pitch_correct::{SCALE_MASKS, SCALE_NAMES};
use cadenza_dsp::{DeviceInfo, ParamInfo};

use crate::i18n::mark;
use crate::state::poly_synth_editor::note_name;

pub const KEY: &str = "key";
pub const SCALE: &str = "scale";
pub const RETUNE: &str = "retune";
pub const HUMANIZE: &str = "humanize";
pub const AMOUNT: &str = "amount";
pub const TRANSPOSE: &str = "transpose";
pub const REFERENCE: &str = "reference";

/// Seconds of pitch history drawn.
pub const TRACE_SECONDS: f64 = 4.0;
/// Fewest semitones the pitch display shows.
pub const MIN_SPAN: f32 = 12.0;
/// The display's range when nothing was sung yet (C3..C5).
pub const IDLE_RANGE: (f32, f32) = (48.0, 72.0);

pub const KEY_NAMES: [&str; 12] = [
	"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The UI names of [`SCALE_NAMES`], same order.
pub const SCALE_LABELS: [&str; 12] = [
	mark("Chromatic"),
	mark("Major"),
	mark("Minor"),
	mark("Harmonic minor"),
	mark("Melodic minor"),
	mark("Dorian"),
	mark("Phrygian"),
	mark("Lydian"),
	mark("Mixolydian"),
	mark("Major pentatonic"),
	mark("Minor pentatonic"),
	mark("Blues"),
];

const _: () = assert!(SCALE_LABELS.len() == SCALE_NAMES.len());

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("pitch_correct").expect("pitch correct is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a pitch_correct param")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcValues {
	pub key: u8,
	pub scale: usize,
	pub retune: f32,
	pub humanize: f32,
	pub amount: f32,
	pub transpose: f32,
	pub reference: f32,
}

impl Default for PcValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl PcValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			key: (f(param(KEY)).round() as u8).min(11),
			scale: (f(param(SCALE)).round() as usize).min(SCALE_MASKS.len() - 1),
			retune: f(param(RETUNE)),
			humanize: f(param(HUMANIZE)),
			amount: f(param(AMOUNT)),
			transpose: f(param(TRANSPOSE)),
			reference: f(param(REFERENCE)),
		}
	}

	/// Whether pitch class `pc` (0 = C) is a target of this key and scale.
	pub fn in_scale(&self, pc: u8) -> bool {
		SCALE_MASKS[self.scale] & (1 << ((pc as i32 - self.key as i32).rem_euclid(12))) != 0
	}

	/// "E phrygian", "Chromatic" (no key: every note).
	pub fn scale_text(&self) -> String {
		let scale = crate::i18n::tr_str(SCALE_LABELS[self.scale]);
		if self.scale == 0 {
			scale.to_owned()
		} else {
			format!("{} {}", KEY_NAMES[self.key as usize], scale.to_lowercase())
		}
	}
}

/// What the device publishes about one moment (fractional MIDI, `None` = unpitched).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchPoint {
	pub time: f64,
	pub detected: Option<f32>,
	pub output: Option<f32>,
	pub target: Option<f32>,
}

impl PitchPoint {
	/// From the probe readings (`readings::PITCH_*`, -1 = unpitched).
	pub fn from_readings(time: f64, detected: f32, output: f32, target: f32) -> Self {
		let some = |v: f32| (v.is_finite() && v > 0.0).then_some(v);
		Self {
			time,
			detected: some(detected),
			output: some(output),
			target: some(target),
		}
	}
}

/// The recent pitch history and the range it is drawn over.
#[derive(Debug, Clone, Default)]
pub struct PitchTrace {
	pub points: VecDeque<PitchPoint>,
	/// Shown semitone range (low, high), eased toward [`PitchTrace::wanted_range`].
	pub range: Option<(f32, f32)>,
}

impl PitchTrace {
	pub fn push(&mut self, p: PitchPoint) {
		if let Some(last) = self.points.back()
			&& p.time < last.time
		{
			self.points.clear();
		}
		self.points.push_back(p);
		while self
			.points
			.front()
			.is_some_and(|f| p.time - f.time > TRACE_SECONDS)
		{
			self.points.pop_front();
		}
	}

	/// The newest pitched point.
	pub fn current(&self) -> Option<&PitchPoint> {
		self.points.back().filter(|p| p.detected.is_some())
	}

	/// Whether anything was sung in the history.
	pub fn heard(&self) -> bool {
		self.points.iter().any(|p| p.detected.is_some())
	}

	/// Semitones covering the history's pitches with a note of margin, at least
	/// [`MIN_SPAN`]; [`IDLE_RANGE`] before anything was sung.
	pub fn wanted_range(&self) -> (f32, f32) {
		let (lo, hi) = self
			.points
			.iter()
			.flat_map(|p| [p.detected, p.output, p.target])
			.flatten()
			.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
		if lo > hi {
			return self.range.unwrap_or(IDLE_RANGE);
		}
		let (lo, hi) = (lo.floor() - 1.0, hi.ceil() + 1.0);
		let pad = ((MIN_SPAN - (hi - lo)) * 0.5).max(0.0);
		((lo - pad).floor(), (hi + pad).ceil())
	}

	/// Eases the shown range toward the wanted one (`k` = share per frame, 1 = jump);
	/// returns it.
	pub fn ease_range(&mut self, k: f32) -> (f32, f32) {
		let want = self.wanted_range();
		let now = match self.range {
			Some((lo, hi)) => (lo + (want.0 - lo) * k, hi + (want.1 - hi) * k),
			None => want,
		};
		self.range = Some(now);
		now
	}
}

/// "A4 +12 c": the nearest note of `pitch` and how far off it is.
pub fn pitch_text(pitch: f32) -> String {
	let note = pitch.round();
	let cents = ((pitch - note) * 100.0).round() as i32;
	let name = note_name(note.clamp(0.0, 127.0) as u8);
	match cents {
		0 => name,
		c if c > 0 => format!("{name} +{c} c"),
		c => format!("{name} \u{2212}{} c", -c),
	}
}

/// The correction applied now, cents (output - detected).
pub fn correction_cents(p: &PitchPoint) -> Option<f32> {
	Some((p.output? - p.detected?) * 100.0)
}

/// "+31 c" for a reference tuning against A4 = 440 Hz ("440 Hz" when standard).
pub fn reference_text(hz: f32) -> String {
	let cents = (1200.0 * (hz / 440.0).log2()).round() as i32;
	match cents {
		0 => format!("{hz:.0} Hz"),
		c if c > 0 => format!("{hz:.1} Hz \u{b7} +{c} c"),
		c => format!("{hz:.1} Hz \u{b7} \u{2212}{} c", -c),
	}
}

/// The reference (Hz) that moves A4 by `cents`.
pub fn reference_for_cents(cents: f32) -> f32 {
	let p = param(REFERENCE);
	(440.0 * 2f32.powf(cents / 1200.0)).clamp(p.min, p.max)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn values_and_scale_membership() {
		let v = PcValues::default();
		assert_eq!(v.scale, 0);
		assert!((0..12).all(|pc| v.in_scale(pc)));
		let e_phrygian = PcValues {
			key: 4,
			scale: 6,
			..v
		};
		let kept: Vec<u8> = (0..12).filter(|&pc| e_phrygian.in_scale(pc)).collect();
		assert_eq!(kept, [0, 2, 4, 5, 7, 9, 11]);
		assert_eq!(e_phrygian.scale_text(), "E phrygian");
		assert_eq!(v.scale_text(), "Chromatic");
	}

	#[test]
	fn the_trace_keeps_four_seconds_and_frames_what_was_sung() {
		let mut t = PitchTrace::default();
		assert_eq!(t.wanted_range(), IDLE_RANGE);
		for i in 0..600 {
			let time = i as f64 * 0.01;
			t.push(PitchPoint::from_readings(time, 64.3, 64.0, 64.0));
		}
		assert!(t.points.front().unwrap().time >= 5.99 - TRACE_SECONDS - 1e-9);
		let (lo, hi) = t.wanted_range();
		assert!(hi - lo >= MIN_SPAN && lo < 63.0 && hi > 66.0, "{lo} {hi}");
		t.push(PitchPoint::from_readings(6.1, -1.0, -1.0, -1.0));
		assert!(t.current().is_none() && t.heard());
		assert_eq!(t.ease_range(1.0), t.wanted_range());
		// The transport jumped back: start over.
		t.push(PitchPoint::from_readings(1.0, 60.0, 60.0, 60.0));
		assert_eq!(t.points.len(), 1);
	}

	#[test]
	fn readouts() {
		assert_eq!(pitch_text(69.12), "A4 +12 c");
		assert_eq!(pitch_text(63.7), "E4 \u{2212}30 c");
		assert_eq!(pitch_text(60.0), "C4");
		let p = PitchPoint::from_readings(0.0, 60.3, 60.05, 60.0);
		assert!((correction_cents(&p).unwrap() + 25.0).abs() < 0.01);
		assert_eq!(reference_text(440.0), "440 Hz");
		assert_eq!(reference_text(448.0), "448.0 Hz \u{b7} +31 c");
		assert!((reference_for_cents(31.0) - 447.95).abs() < 0.05);
	}
}
