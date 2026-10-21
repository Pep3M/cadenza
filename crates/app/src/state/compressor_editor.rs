//! Pure logic of the Compressor editor (`compressor`, design `15 EQ and Compressor`,
//! 15c / 15d): the transfer display's mapping and gestures (threshold dot, ratio drag
//! above the knee, knee wheel, reset), the auto makeup estimate, the sidechain sources
//! a device can be keyed from, and the readout texts. The view turns the changes into
//! one command per gesture.

use crate::i18n::{tr, trf};
use cadenza_core::{DeviceId, TrackId};
use cadenza_dsp::response::{
	KEY_HP_OFF_HZ, KEY_LP_OFF_HZ, compressor_curve_db, compressor_key_response_db,
};
use cadenza_dsp::{DeviceInfo, ParamInfo};
use cadenza_model::{Command, Document, Project, SidechainSource, TrackKind};

pub use crate::state::device::minus;

pub const THRESHOLD: &str = "threshold";
pub const RATIO: &str = "ratio";
pub const ATTACK: &str = "attack";
pub const RELEASE: &str = "release";
pub const KNEE: &str = "knee";
pub const MAKEUP: &str = "makeup";
pub const MIX: &str = "mix";
pub const KEY_HP: &str = "key_hp";
pub const KEY_LP: &str = "key_lp";
/// A key filter node dragged this close (in octaves) to its range end switches off.
const KEY_OFF_SNAP_OCT: f32 = 0.15;

/// Bottom of the transfer display, both axes (the threshold's own minimum).
pub const FLOOR_DB: f32 = -60.0;
/// The nominal program level the auto makeup is estimated at.
pub const NOMINAL_INPUT_DB: f32 = -18.0;
/// Double-click on the transfer display.
pub const RESET_THRESHOLD: f32 = -24.0;
pub const RESET_RATIO: f32 = 4.0;
/// One wheel notch over the knee (Shift: a tenth).
const KNEE_STEP_DB: f32 = 1.0;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("compressor").expect("the compressor is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a compressor param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

/// The values the pictures are drawn from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompValues {
	pub threshold: f32,
	pub ratio: f32,
	pub knee: f32,
	pub makeup: f32,
}

impl Default for CompValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl CompValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			threshold: f(param(THRESHOLD)),
			ratio: f(param(RATIO)),
			knee: f(param(KNEE)),
			makeup: f(param(MAKEUP)),
		}
	}

	/// Output level for a steady input level (the processor's own curve).
	pub fn curve(&self, input_db: f32) -> f32 {
		compressor_curve_db(self.threshold, self.ratio, self.knee, self.makeup, input_db)
	}

	/// Gain reduction (positive dB) for a steady input level.
	pub fn reduction(&self, input_db: f32) -> f32 {
		-(compressor_curve_db(self.threshold, self.ratio, self.knee, 0.0, input_db) - input_db)
	}

	/// The knee's input span (at least a sliver so a hard knee still shows).
	pub fn knee_span(&self) -> (f32, f32) {
		let half = (self.knee * 0.5).max(0.5);
		(self.threshold - half, self.threshold + half)
	}
}

/// 0..1 position of a level on the transfer display's axes.
pub fn db_to_t(db: f32) -> f32 {
	((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

pub fn t_to_db(t: f32) -> f32 {
	FLOOR_DB + t.clamp(0.0, 1.0) * -FLOOR_DB
}

/// What a press on the transfer display grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
	/// The threshold dot: dragged horizontally.
	Threshold,
	/// The curve above the knee: dragged vertically for the ratio.
	Ratio,
	/// Inside the knee shading (the wheel changes the knee).
	Knee,
	None,
}

/// The target under (`in_db`, `out_db`); `tol_db` is the dot's grab radius in dB.
pub fn hit(v: &CompValues, in_db: f32, out_db: f32, tol_db: f32) -> Hit {
	let dot = (v.threshold, v.curve(v.threshold));
	if (in_db - dot.0).hypot(out_db - dot.1) <= tol_db {
		return Hit::Threshold;
	}
	let (lo, hi) = v.knee_span();
	if in_db > hi {
		Hit::Ratio
	} else if in_db >= lo {
		Hit::Knee
	} else {
		Hit::None
	}
}

/// The threshold after a horizontal drag of `delta_db` from `start` (Shift: `fine`).
pub fn drag_threshold(start: f32, delta_db: f32, fine: bool) -> f32 {
	let d = if fine { delta_db * 0.1 } else { delta_db };
	clamp(THRESHOLD, round_to(start + d, 0.1))
}

/// The ratio that puts the curve through `out_db` at the grabbed input `in_db` (above
/// the knee the curve is the straight `threshold + (in - threshold) / ratio + makeup`).
/// A pointer at or below the threshold's level is the steepest ratio, one on the 1:1
/// diagonal (or above it) is 1:1.
pub fn ratio_at(v: &CompValues, in_db: f32, out_db: f32) -> f32 {
	let over = in_db - v.threshold;
	if over < 0.5 {
		return v.ratio;
	}
	let rise = out_db - v.makeup - v.threshold;
	let max = param(RATIO).max;
	if rise <= over / max {
		return max;
	}
	clamp(RATIO, round_to(over / rise, 0.1))
}

/// The knee after `notches` wheel notches (Shift: fine).
pub fn wheel_knee(knee: f32, notches: f32, fine: bool) -> f32 {
	let step = if fine {
		KNEE_STEP_DB * 0.1
	} else {
		KNEE_STEP_DB
	};
	clamp(KNEE, round_to(knee + notches * step, 0.1))
}

/// Double-click: threshold -24 dB, ratio 4:1.
pub fn reset() -> [Change; 2] {
	[(THRESHOLD, RESET_THRESHOLD), (RATIO, RESET_RATIO)]
}

/// The makeup that gives back what the curve takes from a nominal -18 dBFS program
/// level: the static gain reduction there (0 when the threshold sits above it). Peaks
/// get more reduction and quiet passages less, so the middle of the program is the
/// honest single number; rounded to 0.1 dB and kept in the makeup range.
pub fn auto_makeup(v: &CompValues) -> f32 {
	clamp(MAKEUP, round_to(v.reduction(NOMINAL_INPUT_DB), 0.1))
}

/// Whether the suggestion differs enough from the makeup to be worth offering.
pub fn suggests(v: &CompValues) -> Option<f32> {
	let s = auto_makeup(v);
	((s - v.makeup).abs() >= 0.1).then_some(s)
}

fn round_to(v: f32, step: f32) -> f32 {
	(v / step).round() * step
}

/// Whole dB when whole, else one decimal ("−24", "−17.5").
fn short_db(db: f32) -> String {
	if (db - db.round()).abs() < 0.05 {
		minus(format!("{:.0}", db.round()))
	} else {
		minus(format!("{db:.1}"))
	}
}

/// The transfer display's tag: "−24 dB · 4.0:1".
pub fn transfer_tag(v: &CompValues) -> String {
	format!("{} dB · {:.1}:1", short_db(v.threshold), v.ratio)
}

/// The threshold line's tag in the history: "Threshold −24.0 dB".
pub fn threshold_tag(db: f32) -> String {
	minus(trf(
		"Threshold {db} dB",
		&[("db", &crate::i18n::decimal(format!("{:.1}", db)))],
	))
}

/// The gain reduction readout: "−6.2" ("0.0" when nothing is reduced).
pub fn gr_text(gr_db: f32) -> String {
	let gr = gr_db.abs();
	if gr < 0.05 {
		"0.0".to_owned()
	} else {
		minus(format!("{:.1}", -gr))
	}
}

/// "+4.5" / "−1.0".
pub fn signed_db(db: f32) -> String {
	if db.abs() < 0.05 {
		"0.0".to_owned()
	} else {
		minus(format!("{db:+.1}"))
	}
}

/// The key level the history draws: the compressor's `KEY_LEVEL_DB` reading (the real
/// detector signal), floored; no reading (probe idle) = the floor.
pub fn key_level(reading_db: Option<f32>, floor_db: f32) -> f32 {
	reading_db.map_or(floor_db, |db| db.max(floor_db))
}

/// The detector's key filters: (high-pass, low-pass) in Hz, at their range ends when off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyFilter {
	pub hp: f32,
	pub lp: f32,
}

impl KeyFilter {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			hp: f(param(KEY_HP)),
			lp: f(param(KEY_LP)),
		}
	}

	pub fn hp_on(&self) -> bool {
		self.hp > KEY_HP_OFF_HZ + 0.5
	}

	pub fn lp_on(&self) -> bool {
		self.lp < KEY_LP_OFF_HZ - 0.5
	}

	/// The filters' response (the processor's own coefficients).
	pub fn response_db(&self, sample_rate: f32, freq: f32) -> f32 {
		compressor_key_response_db(self.hp, self.lp, sample_rate, freq)
	}

	/// "Key filter off", "Key 120 Hz – 20 kHz", "Key ≥ 120 Hz", "Key ≤ 8 kHz".
	pub fn label(&self) -> String {
		let hz = |f: f32| crate::state::device::format_value(f, "Hz");
		match (self.hp_on(), self.lp_on()) {
			(false, false) => tr("Key filter off").to_owned(),
			(true, false) => trf("Key \u{2265} {freq}", &[("freq", &hz(self.hp))]),
			(false, true) => trf("Key \u{2264} {freq}", &[("freq", &hz(self.lp))]),
			(true, true) => trf(
				"Key {low} \u{2013} {high}",
				&[("low", &hz(self.hp)), ("high", &hz(self.lp))],
			),
		}
	}
}

/// The change a key filter node drag makes (`node` 0 = high-pass, 1 = low-pass): the
/// frequency clamped to the range; near its range end it snaps off.
pub fn drag_key(node: usize, freq: f32) -> Change {
	let id = if node == 0 { KEY_HP } else { KEY_LP };
	let p = param(id);
	let off = if node == 0 { p.min } else { p.max };
	if (freq.max(1.0) / off).log2().abs() < KEY_OFF_SNAP_OCT
		|| (node == 0 && freq < off)
		|| (node == 1 && freq > off)
	{
		return (id, off);
	}
	(id, round_to(freq.clamp(p.min, p.max), 1.0))
}

/// Double-click on a key filter node: off.
pub fn key_off(node: usize) -> Change {
	let id = if node == 0 { KEY_HP } else { KEY_LP };
	(id, param(id).default)
}

/// A track that can key the device.
#[derive(Debug, Clone, PartialEq)]
pub struct KeySource {
	pub track: TrackId,
	pub name: String,
	pub color: Option<String>,
}

/// The tracks that can key `device` (on `owner`, None = master), in display order: every
/// track with audio except the device's own and those the model rejects (feedback
/// loops through outputs, sends, buses or other sidechains). Folders carry no audio.
pub fn key_sources(project: &Project, owner: Option<TrackId>, device: DeviceId) -> Vec<KeySource> {
	let mut doc = Document::new(project.clone());
	project
		.tracks
		.iter()
		.filter(|t| t.kind != TrackKind::Folder && Some(t.id) != owner)
		.filter(|t| {
			doc.execute(Command::SetSidechain {
				track: owner,
				device,
				source: Some(SidechainSource {
					track: t.id,
					pre_fader: false,
				}),
			})
			.is_ok() && doc.undo().is_ok()
		})
		.map(|t| KeySource {
			track: t.id,
			name: t.name.clone(),
			color: t.color.clone(),
		})
		.collect()
}

/// The command a key picker choice makes (`None` = no key).
pub fn set_key(
	owner: Option<TrackId>,
	device: DeviceId,
	track: Option<TrackId>,
	pre_fader: bool,
) -> Command {
	Command::SetSidechain {
		track: owner,
		device,
		source: track.map(|track| SidechainSource { track, pre_fader }),
	}
}

/// "post-fader send" / "pre-fader".
pub fn tap_label(pre_fader: bool) -> &'static str {
	if pre_fader {
		"pre-fader"
	} else {
		tr("post-fader send")
	}
}

#[cfg(test)]
mod tests {
	use cadenza_model::{Device, Output, Track};

	use super::*;

	#[test]
	fn key_filter_nodes_snap_off_at_the_range_ends() {
		let off = KeyFilter::from_fn(|p| p.default);
		assert!(!off.hp_on() && !off.lp_on());
		assert_eq!(off.label(), "Key filter off");
		assert_eq!(off.response_db(48_000.0, 50.0), 0.0);
		assert_eq!(drag_key(0, 150.4), (KEY_HP, 150.0));
		assert_eq!(drag_key(0, 21.0), (KEY_HP, KEY_HP_OFF_HZ));
		assert_eq!(drag_key(0, 5.0), (KEY_HP, KEY_HP_OFF_HZ));
		assert_eq!(drag_key(1, 19_000.0), (KEY_LP, KEY_LP_OFF_HZ));
		assert_eq!(drag_key(1, 8_000.0), (KEY_LP, 8_000.0));
		assert_eq!(drag_key(0, 15_000.0), (KEY_HP, 10_000.0), "kept in range");
		assert_eq!(key_off(0), (KEY_HP, KEY_HP_OFF_HZ));
		let on = KeyFilter {
			hp: 150.0,
			lp: 8_000.0,
		};
		assert!(on.hp_on() && on.lp_on());
		assert!(on.label().starts_with("Key 150 Hz"), "{}", on.label());
		assert!(on.response_db(48_000.0, 40.0) < -15.0);
		let hp = KeyFilter {
			hp: 150.0,
			lp: KEY_LP_OFF_HZ,
		};
		assert!(hp.label().contains('\u{2265}'), "{}", hp.label());
	}

	fn values(threshold: f32, ratio: f32, knee: f32, makeup: f32) -> CompValues {
		CompValues {
			threshold,
			ratio,
			knee,
			makeup,
		}
	}

	#[test]
	fn axes_map_both_ways() {
		assert_eq!(db_to_t(FLOOR_DB), 0.0);
		assert_eq!(db_to_t(0.0), 1.0);
		assert_eq!(db_to_t(12.0), 1.0, "clamped");
		assert!((db_to_t(-30.0) - 0.5).abs() < 1e-6);
		for db in [-60.0, -42.5, -24.0, -3.0, 0.0] {
			assert!((t_to_db(db_to_t(db)) - db).abs() < 1e-4);
		}
	}

	#[test]
	fn curve_matches_the_processor_formula() {
		let v = values(-24.0, 4.0, 6.0, 4.0);
		assert_eq!(v.curve(-40.0), -36.0, "below the knee: unity + makeup");
		assert!((v.curve(-12.0) - (-24.0 + 3.0 + 4.0)).abs() < 1e-4);
		assert!((v.reduction(-12.0) - 9.0).abs() < 1e-4);
		assert_eq!(v.knee_span(), (-27.0, -21.0));
		assert_eq!(values(-24.0, 4.0, 0.0, 0.0).knee_span(), (-24.5, -23.5));
	}

	#[test]
	fn hits_dot_ratio_region_and_knee() {
		let v = values(-24.0, 4.0, 6.0, 0.0);
		let dot = v.curve(-24.0);
		assert_eq!(hit(&v, -24.0, dot, 2.0), Hit::Threshold);
		assert_eq!(hit(&v, -25.0, dot + 1.0, 2.0), Hit::Threshold);
		assert_eq!(
			hit(&v, -10.0, -40.0, 2.0),
			Hit::Ratio,
			"anywhere above the knee"
		);
		assert_eq!(hit(&v, -22.0, -50.0, 1.0), Hit::Knee);
		assert_eq!(hit(&v, -45.0, -45.0, 2.0), Hit::None);
	}

	#[test]
	fn threshold_drag_is_clamped_and_fine() {
		assert_eq!(drag_threshold(-24.0, 6.0, false), -18.0);
		assert!((drag_threshold(-24.0, 6.0, true) - -23.4).abs() < 1e-4);
		assert_eq!(drag_threshold(-24.0, 40.0, false), 0.0);
		assert_eq!(drag_threshold(-24.0, -90.0, false), -60.0);
	}

	#[test]
	fn ratio_follows_the_pointer_above_the_knee() {
		let v = values(-24.0, 2.0, 6.0, 3.0);
		// 12 dB over, the curve 3 dB above the threshold (+ makeup) = 4:1.
		assert!((ratio_at(&v, -12.0, -24.0 + 3.0 + 3.0) - 4.0).abs() < 1e-4);
		assert_eq!(ratio_at(&v, -12.0, -12.0 + 3.0), 1.0, "on the 1:1 line");
		assert_eq!(ratio_at(&v, -12.0, 10.0), 1.0, "above it");
		assert_eq!(
			ratio_at(&v, -12.0, -40.0),
			20.0,
			"below the threshold level"
		);
		assert_eq!(
			ratio_at(&v, -24.2, -50.0),
			2.0,
			"at the threshold: unchanged"
		);
		// Round trip through the curve.
		let w = values(-30.0, 6.0, 2.0, 0.0);
		assert!((ratio_at(&w, -6.0, w.curve(-6.0)) - 6.0).abs() < 0.05);
	}

	#[test]
	fn knee_wheel_steps_and_clamps() {
		assert_eq!(wheel_knee(6.0, 2.0, false), 8.0);
		assert!((wheel_knee(6.0, 2.0, true) - 6.2).abs() < 1e-4);
		assert_eq!(wheel_knee(1.0, -5.0, false), 0.0);
		assert_eq!(wheel_knee(23.0, 5.0, false), 24.0);
		assert_eq!(reset(), [(THRESHOLD, -24.0), (RATIO, 4.0)]);
	}

	#[test]
	fn auto_makeup_is_the_reduction_at_minus_18() {
		// The design's numbers: -24 dB, 4:1, knee 6 -> +4.5.
		let v = values(-24.0, 4.0, 6.0, 4.0);
		assert!((auto_makeup(&v) - 4.5).abs() < 1e-4);
		assert_eq!(suggests(&v), Some(auto_makeup(&v)));
		assert_eq!(
			suggests(&values(-24.0, 4.0, 6.0, 4.5)),
			None,
			"already applied"
		);
		assert_eq!(
			auto_makeup(&values(-10.0, 4.0, 6.0, 0.0)),
			0.0,
			"threshold above"
		);
		// Inside the knee the soft curve's smaller reduction counts.
		let k = auto_makeup(&values(-18.0, 4.0, 12.0, 0.0));
		assert!(k > 0.0 && k < 2.0, "{k}");
		assert_eq!(auto_makeup(&values(-60.0, 20.0, 0.0, 0.0)), 24.0, "clamped");
	}

	#[test]
	fn texts_use_the_typographic_minus() {
		let v = values(-24.0, 4.0, 6.0, 0.0);
		assert_eq!(transfer_tag(&v), "\u{2212}24 dB · 4.0:1");
		assert_eq!(
			transfer_tag(&values(-17.5, 2.5, 0.0, 0.0)),
			"\u{2212}17.5 dB · 2.5:1"
		);
		assert_eq!(threshold_tag(-24.0), "Threshold \u{2212}24.0 dB");
		assert_eq!(gr_text(6.24), "\u{2212}6.2");
		assert_eq!(gr_text(0.01), "0.0");
		assert_eq!(signed_db(4.5), "+4.5");
		assert_eq!(signed_db(-1.0), "\u{2212}1.0");
	}

	#[test]
	fn key_level_reads_the_detector_or_rests_on_the_floor() {
		assert_eq!(key_level(Some(-12.0), -60.0), -12.0);
		assert_eq!(key_level(Some(-144.0), -60.0), -60.0, "silence stays");
		assert_eq!(key_level(None, -60.0), -60.0);
	}

	#[test]
	fn key_sources_skip_the_own_track_folders_and_loops() {
		let mut p = Project::new("Keys");
		let mut bass = Track::new("Bass", TrackKind::Instrument);
		let comp = Device::builtin("compressor", "Compressor");
		let comp_id = comp.id;
		bass.channel.inserts.push(comp);
		let kick = Track::new("Kick", TrackKind::Instrument).with_color("#E8705F");
		let bus = Track::new("Bass bus", TrackKind::Bus);
		bass.channel.output = Output::Bus(bus.id);
		let folder = Track::new("Group", TrackKind::Folder);
		let (bass_id, kick_id) = (bass.id, kick.id);
		p.tracks.extend([bass, kick, bus, folder]);
		let sources = key_sources(&p, Some(bass_id), comp_id);
		let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
		assert_eq!(
			names,
			["Kick"],
			"not itself, the bus it feeds, or the folder"
		);
		assert_eq!(sources[0].color.as_deref(), Some("#E8705F"));

		let mut doc = Document::new(p);
		doc.execute(set_key(Some(bass_id), comp_id, Some(kick_id), true))
			.unwrap();
		let d = doc.project().device(Some(bass_id), comp_id).unwrap();
		assert_eq!(
			d.sidechain,
			Some(SidechainSource {
				track: kick_id,
				pre_fader: true
			})
		);
		doc.execute(set_key(Some(bass_id), comp_id, None, false))
			.unwrap();
		assert!(
			doc.project()
				.device(Some(bass_id), comp_id)
				.unwrap()
				.sidechain
				.is_none()
		);
		assert_eq!(tap_label(false), "post-fader send");
	}
}
