//! Pure logic of the Poly Synth editor (`poly_synth`, design `17 Instruments`, 17a): the
//! wave cards, the osc 2 tuning drags, the filter node (x = cutoff, a vertical drag =
//! resonance, wheel = env amount) and its env-peak curve, the two envelopes' parameter
//! bindings and readouts, the trigger-aligned output scope, and the on-screen / computer
//! keyboard mapping (FL-style A–K). The view turns the changes into one command per
//! gesture.

use crate::i18n::{mark, tr, trf};
use cadenza_dsp::{DeviceInfo, ParamInfo};

use crate::state::device::{KnobSpec, format_value};
use crate::state::device_display::{AdsrRanges, AdsrStage, AdsrValues, short_hz};

pub const OSC1_WAVE: &str = "osc1_wave";
pub const OSC2_WAVE: &str = "osc2_wave";
pub const OSC2_SEMI: &str = "osc2_semitones";
pub const OSC2_CENTS: &str = "osc2_cents";
pub const OSC_MIX: &str = "osc_mix";
pub const UNISON: &str = "unison";
pub const UNISON_DETUNE: &str = "unison_detune";
pub const NOISE: &str = "noise";
pub const CUTOFF: &str = "filter_cutoff";
pub const RESONANCE: &str = "filter_resonance";
pub const SLOPE: &str = "filter_slope";
pub const ENV_AMOUNT: &str = "filter_env_amount";
pub const KEY_TRACK: &str = "filter_key_track";
pub const VEL_SENS: &str = "velocity_sensitivity";
pub const GLIDE: &str = "glide";
pub const GAIN: &str = "gain";

pub const OSC1_TABLE: &str = "osc1_table";
pub const OSC2_TABLE: &str = "osc2_table";
pub const OSC1_POSITION: &str = "osc1_position";
pub const OSC2_POSITION: &str = "osc2_position";

/// The waves in parameter order (0 = sine ... 3 = triangle, 4 = wavetable).
pub const WAVE_NAMES: [&str; 5] = [
	mark("Sine"),
	mark("Saw"),
	mark("Square"),
	mark("Triangle"),
	mark("Wavetable"),
];
/// The wave value of the wavetable oscillator.
pub const WAVETABLE: usize = cadenza_dsp::response::WAVE_WAVETABLE;
/// The built-in tables in `oscN_table` order (names as `cadenza_dsp` lists them).
pub const TABLE_NAMES: [&str; 5] = [
	mark("Basic"),
	mark("PWM"),
	mark("Formant"),
	mark("Harmonics"),
	mark("Digital"),
];
/// The oscillators' table and position parameters, osc 1 first.
pub const TABLE_PARAMS: [&str; 2] = [OSC1_TABLE, OSC2_TABLE];
pub const POSITION_PARAMS: [&str; 2] = [OSC1_POSITION, OSC2_POSITION];
/// A vertical drag over the wavetable picture sweeps the whole table in this many points.
pub const POSITION_PX: f32 = 160.0;
/// One wheel notch over the picture moves the position this much.
pub const POSITION_STEP: f32 = 0.02;
/// Frames the waterfall picture draws behind the current one.
pub const WATERFALL_FRAMES: usize = 9;
/// The oscillators' wave parameters, osc 1 first.
pub const WAVE_PARAMS: [&str; 2] = [OSC1_WAVE, OSC2_WAVE];
/// The filter display draws the response at this rate (the engine default).
pub const DISPLAY_RATE: f32 = 48_000.0;
/// A vertical drag on the semitones readout moves one semitone per this many points.
pub const SEMI_PX: f32 = 6.0;
/// ... and on the cents readout one cent per this many points.
pub const CENTS_PX: f32 = 1.5;
/// A full-height drag on the filter display sweeps resonance by this much (%).
pub const RES_PER_DB: f32 = 100.0 / 48.0;
/// Env amount moves this many semitones per wheel notch over the filter node.
pub const ENV_STEP: f32 = 1.0;
/// Points the output scope shows.
pub const SCOPE_POINTS: usize = 300;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("poly_synth").expect("the poly synth is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a poly synth param")
}

pub fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

pub fn default(id: &str) -> f32 {
	param(id).default
}

// --- Oscillators ----------------------------------------------------------------------

/// The wave index (0..=4) a wave parameter value selects.
pub fn wave_index(value: f32) -> usize {
	(value.round().clamp(0.0, WAVETABLE as f32)) as usize
}

/// The table index (0..=4) a table parameter value selects.
pub fn table_index(value: f32) -> usize {
	(value.round().clamp(0.0, (TABLE_NAMES.len() - 1) as f32)) as usize
}

/// The oscillators (0, 1) playing a wavetable, given each one's wave value.
pub fn wavetable_oscs(waves: [f32; 2]) -> Vec<usize> {
	(0..2)
		.filter(|&o| wave_index(waves[o]) == WAVETABLE)
		.collect()
}

/// The position after a vertical drag of `dy` points from `start` (up = later frames;
/// `fine`: ×0.1).
pub fn drag_position(start: f32, dy: f32, fine: bool) -> f32 {
	let scale = if fine { 0.1 } else { 1.0 };
	(start - dy / POSITION_PX * scale).clamp(0.0, 1.0)
}

/// The position after `notches` wheel notches from `start`.
pub fn wheel_position(start: f32, notches: f32) -> f32 {
	((start + notches * POSITION_STEP) * 1000.0)
		.round()
		.clamp(0.0, 1000.0)
		/ 1000.0
}

/// The frames the waterfall draws, back (position 1) to front (position 0): each one's
/// position and `points` samples of its cycle, read from the synth's own tables.
pub fn waterfall(table: usize, points: usize) -> Vec<(f32, Vec<f32>)> {
	(0..WATERFALL_FRAMES)
		.rev()
		.map(|i| {
			let pos = i as f32 / (WATERFALL_FRAMES - 1) as f32;
			(pos, frame(table, pos, points))
		})
		.collect()
}

/// One cycle of `table` at `position`, `points` samples (what the oscillator plays).
pub fn frame(table: usize, position: f32, points: usize) -> Vec<f32> {
	let mut out = vec![0.0; points];
	cadenza_dsp::response::wavetable_frame(table, position, &mut out);
	out
}

/// "Formant · 0.42": the readout under the picture.
pub fn position_tag(table: usize, position: f32) -> String {
	format!(
		"{} · {}",
		tr(TABLE_NAMES[table.min(TABLE_NAMES.len() - 1)]),
		crate::i18n::num(position as f64, 2)
	)
}

/// "Copy to other osc": the other oscillator's wave set to `osc`'s.
pub fn copy_wave(osc: usize, value: f32) -> Change {
	(WAVE_PARAMS[1 - osc.min(1)], wave_index(value) as f32)
}

/// The semitones after a vertical drag of `dy` points from `start` (up = higher).
pub fn drag_semitones(start: f32, dy: f32) -> f32 {
	clamp(OSC2_SEMI, (start - dy / SEMI_PX).round())
}

/// The cents after a vertical drag of `dy` points from `start` (`fine`: ×0.1).
pub fn drag_cents(start: f32, dy: f32, fine: bool) -> f32 {
	let scale = if fine { 0.1 } else { 1.0 };
	clamp(OSC2_CENTS, (start - dy / CENTS_PX * scale).round())
}

// --- Filter ---------------------------------------------------------------------------

/// The filter's magnitude at `freq` as the voice applies it.
pub fn response_db(cutoff: f32, resonance: f32, slope_24: bool, freq: f32) -> f32 {
	cadenza_dsp::response::poly_synth_filter_response_db(
		cutoff,
		resonance,
		slope_24,
		DISPLAY_RATE,
		freq,
	)
}

/// Where the envelope opens the cutoff to at its peak: `env` semitones above it.
pub fn env_peak_cutoff(cutoff: f32, env: f32) -> f32 {
	(cutoff * 2f32.powf(env / 12.0)).clamp(20.0, 20_000.0)
}

/// The node's start for a drag (the values when it was grabbed, and its dB on screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeStart {
	pub resonance: f32,
	pub db: f32,
}

/// The node dragged to `freq` / `db`: cutoff follows x, resonance the vertical travel
/// since the grab (the node rides the curve, so its own dB is not the resonance).
pub fn node_drag(start: NodeStart, freq: f32, db: f32) -> [Change; 2] {
	let res = start.resonance + (db - start.db) * RES_PER_DB;
	[
		(CUTOFF, clamp(CUTOFF, freq)),
		(RESONANCE, clamp(RESONANCE, (res * 10.0).round() / 10.0)),
	]
}

/// Double-click on the node: cutoff and resonance back to their defaults.
pub fn reset_node() -> [Change; 2] {
	[(CUTOFF, default(CUTOFF)), (RESONANCE, default(RESONANCE))]
}

/// Env amount after `notches` wheel notches from `start`.
pub fn wheel_env(start: f32, notches: f32) -> f32 {
	clamp(ENV_AMOUNT, (start + notches * ENV_STEP).round())
}

/// The node's tag: "1.8 kHz · res 45 %".
pub fn filter_tag(cutoff: f32, resonance: f32) -> String {
	format!("{} \u{b7} res {:.0} %", short_hz(cutoff), resonance)
}

/// "+24 st" / "−12 st" for the dashed curve's caption.
pub fn env_label(env: f32) -> String {
	crate::state::device::minus(format_value(env.round(), "semitones"))
}

// --- Envelopes ------------------------------------------------------------------------

/// The amplitude or the filter envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
	Amp,
	Filter,
}

impl Env {
	pub const ALL: [Env; 2] = [Env::Amp, Env::Filter];

	pub fn title(self) -> &'static str {
		match self {
			Env::Amp => tr("AMP ENVELOPE"),
			Env::Filter => tr("FILTER ENVELOPE"),
		}
	}

	/// The parameter `stage` of this envelope edits.
	pub fn param(self, stage: AdsrStage) -> &'static str {
		match (self, stage) {
			(Env::Amp, AdsrStage::Attack) => "amp_attack",
			(Env::Amp, AdsrStage::Decay) => "amp_decay",
			(Env::Amp, AdsrStage::Sustain) => "amp_sustain",
			(Env::Amp, AdsrStage::Release) => "amp_release",
			(Env::Filter, AdsrStage::Attack) => "filter_attack",
			(Env::Filter, AdsrStage::Decay) => "filter_decay",
			(Env::Filter, AdsrStage::Sustain) => "filter_sustain",
			(Env::Filter, AdsrStage::Release) => "filter_release",
		}
	}

	pub fn ranges(self) -> AdsrRanges {
		let spec = |stage| {
			let p = param(self.param(stage));
			KnobSpec::new(p.min, p.max, p.default)
		};
		AdsrRanges {
			attack: spec(AdsrStage::Attack),
			decay: spec(AdsrStage::Decay),
			sustain: spec(AdsrStage::Sustain),
			release: spec(AdsrStage::Release),
			sustain_unit: "%",
		}
	}

	pub fn values(self, value: impl Fn(&str) -> f32) -> AdsrValues {
		AdsrValues {
			attack: value(self.param(AdsrStage::Attack)),
			decay: value(self.param(AdsrStage::Decay)),
			sustain: value(self.param(AdsrStage::Sustain)),
			release: value(self.param(AdsrStage::Release)),
		}
	}

	/// The undo step's name for a node gesture that moved `stages`.
	pub fn label(self, stages: &[AdsrStage]) -> String {
		let env = match self {
			Env::Amp => tr("Amp"),
			Env::Filter => tr("Filter"),
		};
		let names: Vec<&str> = stages.iter().map(|s| stage_name(*s)).collect();
		format!("{env} {}", names.join(" and ").to_lowercase())
	}
}

pub fn stage_name(stage: AdsrStage) -> &'static str {
	match stage {
		AdsrStage::Attack => tr("Attack"),
		AdsrStage::Decay => tr("Decay"),
		AdsrStage::Sustain => tr("Sustain"),
		AdsrStage::Release => tr("Release"),
	}
}

/// The readouts under an envelope: ("Attack", "350 ms"), ("Decay", "1.20 s")...
pub fn readouts(v: &AdsrValues) -> [(&'static str, String); 4] {
	[
		(tr("Attack"), format_value(v.attack, "ms")),
		(tr("Decay"), format_value(v.decay, "ms")),
		(tr("Sustain"), format_value(v.sustain, "%")),
		(tr("Release"), format_value(v.release, "ms")),
	]
}

// --- Scope ----------------------------------------------------------------------------

/// `n` samples starting at the latest rising zero crossing that still leaves `n`
/// samples after it, so a periodic signal stands still from frame to frame. Without a
/// crossing: the newest `n`; a shorter input is padded with silence in front.
pub fn scope_window(samples: &[f32], n: usize) -> Vec<f32> {
	if samples.len() <= n {
		let mut out = vec![0.0; n - samples.len()];
		out.extend_from_slice(samples);
		return out;
	}
	let last = samples.len() - n;
	let start = (1..=last)
		.rev()
		.find(|&i| samples[i - 1] < 0.0 && samples[i] >= 0.0)
		.unwrap_or(last);
	samples[start..start + n].to_vec()
}

/// The scope's vertical scale: the loudest sample reaches 90 % of the half height;
/// quiet material is magnified at most 8×.
pub fn scope_gain(samples: &[f32]) -> f32 {
	let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
	(0.9 / peak.max(0.9 / 8.0)).min(8.0)
}

// --- Keyboard -------------------------------------------------------------------------

/// The lowest base the octave shift reaches (C1) and the highest (C7).
pub const BASE_MIN: u8 = 24;
pub const BASE_MAX: u8 = 96;
/// The keyboard starts at C3.
pub const BASE_DEFAULT: u8 = 48;
/// White keys per octave and their semitone offsets.
const WHITE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

pub fn is_black(note: u8) -> bool {
	matches!(note % 12, 1 | 3 | 6 | 8 | 10)
}

/// "C3", "F#4" (C4 = 60).
pub fn note_name(note: u8) -> String {
	const NAMES: [&str; 12] = [
		"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
	];
	format!("{}{}", NAMES[note as usize % 12], note as i32 / 12 - 1)
}

/// FL-style computer keys: A S D F G H J K = C D E F G A B C, W E T Y U = the black
/// keys; returns the semitone above the base.
pub fn computer_key(c: char) -> Option<u8> {
	Some(match c.to_ascii_lowercase() {
		'a' => 0,
		'w' => 1,
		's' => 2,
		'e' => 3,
		'd' => 4,
		'f' => 5,
		't' => 6,
		'g' => 7,
		'y' => 8,
		'h' => 9,
		'u' => 10,
		'j' => 11,
		'k' => 12,
		_ => return None,
	})
}

/// Z = octave down, X = octave up.
pub fn octave_key(c: char) -> Option<i8> {
	match c.to_ascii_lowercase() {
		'z' => Some(-1),
		'x' => Some(1),
		_ => None,
	}
}

/// The base after shifting `octaves` octaves (kept between C1 and C7).
pub fn shift_base(base: u8, octaves: i8) -> u8 {
	(base as i32 + 12 * octaves as i32).clamp(BASE_MIN as i32, BASE_MAX as i32) as u8
}

/// The note a computer key plays at `base`.
pub fn computer_note(base: u8, c: char) -> Option<u8> {
	computer_key(c)
		.map(|o| base as u16 + o as u16)
		.filter(|n| *n <= 127)
		.map(|n| n as u8)
}

/// The white keys of `octaves` octaves from `base` (a C), left to right.
pub fn white_keys(base: u8, octaves: usize) -> Vec<u8> {
	(0..octaves * 7)
		.map(|i| base + 12 * (i / 7) as u8 + WHITE[i % 7])
		.filter(|n| *n <= 127)
		.collect()
}

/// The black keys of `octaves` octaves from `base` with their left edge in white-key
/// widths.
pub fn black_keys(base: u8, octaves: usize) -> Vec<(u8, f32)> {
	let mut out = Vec::with_capacity(octaves * 5);
	for o in 0..octaves {
		for (semi, white) in [(1u8, 0usize), (3, 1), (6, 3), (8, 4), (10, 5)] {
			let note = base as usize + 12 * o + semi as usize;
			if note <= 127 {
				out.push((note as u8, (white + o * 7) as f32 + BLACK_OFFSET));
			}
		}
	}
	out
}

/// A black key's width and left edge inside its white key, in white-key widths.
pub const BLACK_WIDTH: f32 = 0.6;
const BLACK_OFFSET: f32 = 1.0 - BLACK_WIDTH * 0.5;
/// Black keys reach this far down the white ones.
pub const BLACK_DEPTH: f32 = 0.6;

/// The key under (`x`, `y`) in white-key widths across and 0..1 down the keyboard.
pub fn key_at(base: u8, octaves: usize, x: f32, y: f32) -> Option<u8> {
	if x < 0.0 || !(0.0..=1.0).contains(&y) {
		return None;
	}
	if y <= BLACK_DEPTH
		&& let Some((note, _)) = black_keys(base, octaves)
			.into_iter()
			.find(|(_, left)| x >= *left && x < left + BLACK_WIDTH)
	{
		return Some(note);
	}
	white_keys(base, octaves).get(x as usize).copied()
}

/// The scope's caption: "Scope · C3 E3 G3" (held notes low to high, at most 6).
pub fn scope_label(notes: &[u8]) -> String {
	let mut notes = notes.to_vec();
	notes.sort_unstable();
	notes.dedup();
	if notes.is_empty() {
		return tr("Scope").to_owned();
	}
	let names: Vec<String> = notes.iter().take(6).map(|n| note_name(*n)).collect();
	let more = if notes.len() > 6 { " \u{2026}" } else { "" };
	trf(
		"Scope · {names}{more}",
		&[("names", &names.join(" ")), ("more", &more)],
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn scope_window_starts_on_a_rising_zero_crossing() {
		let tau = std::f32::consts::TAU;
		let sine: Vec<f32> = (0..2000)
			.map(|i| (tau * i as f32 / 97.0 + 1.0).sin())
			.collect();
		let w = scope_window(&sine, 300);
		assert_eq!(w.len(), 300);
		assert!(
			w[0] >= 0.0 && w[0] < 0.1,
			"starts at the crossing: {}",
			w[0]
		);
		assert!(w[1] > w[0], "rising");
		// The latest crossing leaving 300 samples: within one period of the end.
		let start = sine.len() - 300;
		let at = (0..=start).rev().find(|&i| sine[i] == w[0]).unwrap();
		assert!(start - at < 97);
	}

	#[test]
	fn scope_window_pads_short_input_and_falls_back_to_the_tail() {
		assert_eq!(scope_window(&[0.5, 0.25], 4), vec![0.0, 0.0, 0.5, 0.25]);
		let dc = vec![0.3; 500];
		assert_eq!(scope_window(&dc, 300), vec![0.3; 300]);
		let ramp: Vec<f32> = (0..400).map(|i| i as f32).collect();
		assert_eq!(scope_window(&ramp, 300)[0], 100.0);
	}

	#[test]
	fn scope_gain_magnifies_quiet_signals_up_to_a_limit() {
		assert!((scope_gain(&[0.0, -0.9, 0.3]) - 1.0).abs() < 1e-6);
		assert_eq!(scope_gain(&[0.001]), 8.0);
		assert_eq!(scope_gain(&[]), 8.0);
	}

	#[test]
	fn computer_keys_map_fl_style() {
		let notes: Vec<u8> = "awsedftgyhujk"
			.chars()
			.map(|c| computer_note(48, c).unwrap())
			.collect();
		assert_eq!(notes, (48..=60).collect::<Vec<_>>());
		assert_eq!(computer_note(48, 'A'), Some(48));
		assert_eq!(computer_note(48, 'q'), None);
		assert_eq!(octave_key('z'), Some(-1));
		assert_eq!(octave_key('X'), Some(1));
		assert_eq!(shift_base(48, -1), 36);
		assert_eq!(shift_base(BASE_MIN, -1), BASE_MIN);
		assert_eq!(shift_base(BASE_MAX, 1), BASE_MAX);
	}

	#[test]
	fn keyboard_layout_and_hit_testing() {
		let whites = white_keys(48, 3);
		assert_eq!(whites.len(), 21);
		assert_eq!(&whites[..8], &[48, 50, 52, 53, 55, 57, 59, 60]);
		assert!(whites.iter().all(|n| !is_black(*n)));
		let blacks = black_keys(48, 3);
		assert_eq!(blacks.len(), 15);
		assert!(blacks.iter().all(|(n, _)| is_black(*n)));
		// C#3 straddles the C3 / D3 boundary.
		assert_eq!(blacks[0].0, 49);
		assert!((blacks[0].1 - 0.7).abs() < 1e-6);
		assert_eq!(key_at(48, 3, 0.2, 0.9), Some(48), "low on C");
		assert_eq!(key_at(48, 3, 0.95, 0.3), Some(49), "high on the black key");
		assert_eq!(key_at(48, 3, 0.95, 0.8), Some(48), "below the black key");
		assert_eq!(
			key_at(48, 3, 2.5, 0.3),
			Some(52),
			"E has no black key above it"
		);
		assert_eq!(key_at(48, 3, 21.5, 0.5), None);
		assert_eq!(key_at(48, 3, 1.5, 1.5), None);
		assert_eq!(note_name(48), "C3");
		assert_eq!(note_name(61), "C#4");
		assert_eq!(scope_label(&[55, 48, 52, 48]), "Scope \u{b7} C3 E3 G3");
		assert_eq!(scope_label(&[]), "Scope");
	}

	#[test]
	fn the_filter_node_maps_x_to_cutoff_and_travel_to_resonance() {
		let start = NodeStart {
			resonance: 40.0,
			db: -3.0,
		};
		let [(c, cutoff), (r, res)] = node_drag(start, 1800.0, -3.0 + 12.0);
		assert_eq!((c, r), (CUTOFF, RESONANCE));
		assert_eq!(cutoff, 1800.0);
		assert!((res - 65.0).abs() < 0.05, "12 dB up = +25 %: {res}");
		let [(_, cutoff), (_, res)] = node_drag(start, 5.0, -60.0);
		assert_eq!((cutoff, res), (20.0, 0.0), "clamped to the ranges");
		assert_eq!(reset_node(), [(CUTOFF, 2500.0), (RESONANCE, 15.0)]);
		assert_eq!(wheel_env(24.0, 3.0), 27.0);
		assert_eq!(wheel_env(59.0, 4.0), 60.0);
		assert_eq!(filter_tag(1800.0, 45.0), "1.8 kHz \u{b7} res 45 %");
		assert!((env_peak_cutoff(1000.0, 12.0) - 2000.0).abs() < 0.5);
		assert_eq!(env_peak_cutoff(15_000.0, 24.0), 20_000.0);
		assert_eq!(env_label(-12.0), "\u{2212}12 st");
	}

	#[test]
	fn the_filter_response_has_its_resonance_peak_at_the_cutoff() {
		let flat = response_db(1000.0, 0.0, true, 1000.0);
		let peaky = response_db(1000.0, 80.0, true, 1000.0);
		assert!(peaky > flat + 6.0);
		assert!(response_db(1000.0, 0.0, true, 8000.0) < -30.0, "24 dB/oct");
		assert!(response_db(1000.0, 0.0, false, 8000.0) > response_db(1000.0, 0.0, true, 8000.0));
	}

	#[test]
	fn osc_tuning_drags_and_wave_copy() {
		assert_eq!(drag_semitones(7.0, -SEMI_PX * 2.0), 9.0, "up = higher");
		assert_eq!(drag_semitones(20.0, -SEMI_PX * 10.0), 24.0);
		assert_eq!(drag_cents(-6.0, CENTS_PX * 4.0, false), -10.0);
		assert_eq!(drag_cents(0.0, -CENTS_PX * 30.0, true), 3.0);
		assert_eq!(copy_wave(0, 1.0), (OSC2_WAVE, 1.0));
		assert_eq!(copy_wave(1, 2.6), (OSC1_WAVE, 3.0));
		assert_eq!(wave_index(-1.0), 0);
		assert_eq!(wave_index(4.2), WAVETABLE);
	}

	#[test]
	fn wavetable_position_gestures_and_waterfall() {
		assert_eq!(wavetable_oscs([4.0, 1.0]), [0]);
		assert_eq!(wavetable_oscs([4.0, 4.0]), [0, 1]);
		assert!(wavetable_oscs([0.0, 3.0]).is_empty());
		assert_eq!(
			drag_position(0.5, -POSITION_PX * 0.25, false),
			0.75,
			"up = later"
		);
		assert_eq!(drag_position(0.9, -POSITION_PX, false), 1.0);
		assert!((drag_position(0.5, POSITION_PX, true) - 0.4).abs() < 1e-6);
		assert_eq!(wheel_position(0.5, 3.0), 0.56);
		assert_eq!(wheel_position(0.01, -2.0), 0.0);
		assert_eq!(table_index(7.0), 4);
		let frames = waterfall(0, 64);
		assert_eq!(frames.len(), WATERFALL_FRAMES);
		assert_eq!(frames[0].0, 1.0, "back frame first");
		assert_eq!(frames.last().unwrap().0, 0.0);
		// Basic: the front frame is the sine the DSP plays.
		let sine = &frames.last().unwrap().1;
		assert!((sine[16] - 1.0).abs() < 0.02 && sine[0].abs() < 0.02);
		assert!(position_tag(2, 0.4).starts_with("Formant"));
	}

	#[test]
	fn envelopes_bind_their_params_and_name_their_steps() {
		let amp = Env::Amp.ranges();
		assert_eq!(amp.release.max, 20_000.0);
		assert_eq!(Env::Filter.param(AdsrStage::Decay), "filter_decay");
		let v = Env::Amp.values(|id| match id {
			"amp_attack" => 350.0,
			"amp_decay" => 1200.0,
			"amp_sustain" => 70.0,
			_ => 2400.0,
		});
		let r = readouts(&v);
		assert_eq!(r[0], ("Attack", "350 ms".to_owned()));
		assert_eq!(r[1], ("Decay", "1.20 s".to_owned()));
		assert_eq!(r[2], ("Sustain", "70 %".to_owned()));
		assert_eq!(r[3], ("Release", "2.40 s".to_owned()));
		assert_eq!(
			Env::Amp.label(&[AdsrStage::Decay, AdsrStage::Sustain]),
			"Amp decay and sustain"
		);
		assert_eq!(Env::Filter.label(&[AdsrStage::Attack]), "Filter attack");
	}
}
